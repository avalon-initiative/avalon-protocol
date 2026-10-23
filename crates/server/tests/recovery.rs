//! Exercises the social-recovery flow against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Identities/sessions/friendships are seeded directly via SQL, same
//! approach `crates/server/tests/friends.rs` and `device_grants.rs` take —
//! this file is scoped to what #201 adds, not to re-proving the WebAuthn
//! ceremony itself (already covered by `crates/server/src/auth.rs`'s
//! in-process test and `passkeys.rs`'s integration test). The one place a
//! *real* WebAuthn ceremony is unavoidable is the recovery request itself
//! (`recovery::start_request`/`finish_request` genuinely drive
//! `Webauthn::start_passkey_registration`), so that part uses the same
//! virtual-authenticator client `passkeys.rs` already established.
//!
//! The mandatory public time-delay is not waited out in real time. It is
//! measured between cosigned tree heads, so the tests that need it to have
//! run add a later tree head to the node's own `signed_tree_heads` (see
//! `elapse_delay`) instead of sleeping for 48 hours, and remove it again.

mod chain_sign;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
fn sign_action(signing_key: &SigningKey, action_tag: &str, fields: &[&str]) -> String {
    let mut message = format!("avalon:{action_tag}:v1");
    for field in fields {
        message.push(':');
        message.push_str(field);
    }
    BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
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
        .expect("failed to connect to Postgres — is it reachable?")
}

type VirtualClient = Client<
    MemoryStore,
    MockUserValidationMethod,
    passkey_crypto::AvailableBackend,
    public_suffix::PublicSuffixList,
    (),
>;

/// Unlike `passkeys.rs`'s/`authenticate.rs`'s version of this helper, a
/// virtual client here only ever drives one ceremony (`initiate_recovery`'s
/// `.register()` for the recovering device) — the owner and guardians never
/// touch WebAuthn at all (see `seed_identity_session`'s doc comment) — so
/// `verified_user` expects exactly 1 `check_user` call, not 2.
fn new_virtual_client() -> VirtualClient {
    let authenticator = Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(1),
        passkey_crypto::AvailableBackend,
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

/// Seeds a bare identity + profile + session, bypassing WebAuthn entirely
/// — same approach `friends.rs::seed_identity_session` takes. Used for the
/// owner and every guardian, none of which need a real passkey for these
/// tests (only the *recovering device* does, since it's the one thing the
/// server genuinely runs a WebAuthn ceremony for).
async fn seed_identity_session(pool: &PgPool) -> (avalon_protocol::ids::IdentityId, String) {
    let who = avalon_protocol::identity_id::TestIdentity::new();
    let identity_id = who.id;
    sqlx::query("INSERT INTO identities (id, inception_public_key) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(who.public_key().to_vec())
        .execute(pool)
        .await
        .expect("failed to seed identity");
    chain_sign::register(pool, &who).await;
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("recovery-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token_hash, identity_id, expires_at) VALUES (sha256(convert_to($1::text, 'UTF8')), $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

async fn seed_friendship(
    pool: &PgPool,
    a: avalon_protocol::ids::IdentityId,
    b: avalon_protocol::ids::IdentityId,
) {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    sqlx::query("INSERT INTO indexer_friendships (a, b, since) VALUES ($1, $2, now())")
        .bind(lo)
        .bind(hi)
        .execute(pool)
        .await
        .expect("failed to seed friendship");
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

/// The request once its marker is in a ledger entry (guardians approve against that entry).
async fn committed(http: &reqwest::Client, base: &str, request_id: &str) -> serde_json::Value {
    for _ in 0..300 {
        let request: serde_json::Value = http
            .get(format!("{base}/recovery/requests/{request_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if !request["request_entry_hash"].is_null() {
            return request;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the request marker was never committed to the ledger");
}

fn hash32(text: &serde_json::Value) -> [u8; 32] {
    hex::decode(text.as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

/// Drives a real recovery-initiation ceremony (`start`/`finish`) for
/// `identity_id` and returns the resulting `recovery_requests` row id plus
/// its JSON body.
async fn initiate_recovery(
    http: &reqwest::Client,
    base: &str,
    identity_id: avalon_protocol::ids::IdentityId,
) -> (serde_json::Value, SigningKey) {
    let new_key = SigningKey::generate(&mut rand::rng());
    let start: serde_json::Value = http
        .post(format!("{base}/recovery/requests/start"))
        .json(&serde_json::json!({ "identity_id": identity_id, "device_label": "new phone" }))
        .send()
        .await
        .expect("recovery/requests/start failed — is `make start` running?")
        .error_for_status()
        .expect("recovery/requests/start should succeed")
        .json()
        .await
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();

    let mut client = new_virtual_client();
    let origin = rp_origin();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let credential = client
        .register(Origin::from(&origin), creation_options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");

    let request_id = Uuid::new_v4();
    let request_bytes = avalon_protocol::identity_id::recovery_request_signing_bytes(
        &chain_sign::network_id(),
        &identity_id,
        request_id,
        &new_key.verifying_key().to_bytes(),
        &hash32(&start["config_event_hash"]),
    );
    let request = http
        .post(format!("{base}/recovery/requests/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
            "webauthn_credential": credential,
            "new_signing_public_key": BASE64.encode(new_key.verifying_key().to_bytes()),
            "request_id": request_id,
            "signature": BASE64.encode(new_key.sign(&request_bytes).to_bytes()),
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("recovery/requests/finish should succeed")
        .json()
        .await
        .unwrap();
    (request, new_key)
}

/// A guardian's approval body: their own active key, their chain position and a signature over the
/// request, its new key, the request marker's ledger entry and the configuration relied on.
async fn approval(
    http: &reqwest::Client,
    base: &str,
    owner: avalon_protocol::ids::IdentityId,
    guardian: avalon_protocol::ids::IdentityId,
    request_id: &str,
    new_key: &SigningKey,
) -> serde_json::Value {
    let request = committed(http, base, request_id).await;
    let (key, key_id) = chain_sign::key_of(guardian);
    let (head_seq, head_hash) = chain_sign::head(guardian).await;
    let prev: Option<[u8; 32]> = head_hash.as_deref().map(|h| hash32(&h.into()));
    let bytes = avalon_protocol::identity_id::recovery_approval_signing_bytes(
        &chain_sign::network_id(),
        &owner,
        request_id.parse().unwrap(),
        &guardian,
        key_id,
        &new_key.verifying_key().to_bytes(),
        &avalon_protocol::identity_id::RecoveryApprovalBinding {
            request_entry_hash: &hash32(&request["request_entry_hash"]),
            config_event_hash: &hash32(&request["config_event_hash"]),
            guardian_seq: head_seq + 1,
            guardian_prev_hash: prev.as_ref(),
        },
    );
    serde_json::json!({
        "signing_key_id": key_id,
        "seq": head_seq + 1,
        "prev_hash": head_hash,
        "signature": BASE64.encode(key.sign(&bytes).to_bytes()),
    })
}

/// A guardian's signed withdrawal body for their approval of `request_id`.
async fn withdrawal(
    owner: avalon_protocol::ids::IdentityId,
    guardian: avalon_protocol::ids::IdentityId,
    request_id: &str,
) -> serde_json::Value {
    let (key, key_id) = chain_sign::key_of(guardian);
    let (head_seq, head_hash) = chain_sign::head(guardian).await;
    let prev: Option<[u8; 32]> = head_hash.as_deref().map(|h| hash32(&h.into()));
    let bytes = avalon_protocol::identity_id::recovery_withdrawal_signing_bytes(
        &chain_sign::network_id(),
        &owner,
        request_id.parse().unwrap(),
        &guardian,
        key_id,
        head_seq + 1,
        prev.as_ref(),
    );
    serde_json::json!({
        "signing_key_id": key_id,
        "seq": head_seq + 1,
        "prev_hash": head_hash,
        "signature": BASE64.encode(key.sign(&bytes).to_bytes()),
    })
}

/// The owner's veto body: the chained, signed `identity.recovery_cancelled`.
async fn cancel_body(
    owner: avalon_protocol::ids::IdentityId,
    request_id: &str,
    reason: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "reason": reason,
        "chain_event": chain_sign::recovery_cancelled(owner, request_id.parse().unwrap(), reason).await,
    })
}

/// Makes the delay of `request_id` run in cosigned-head time: waits for a tree head covering the
/// request marker, then adds a later head 49 hours after it. Returns that head's size for
/// [`remove_head`].
async fn elapse_delay(pool: &PgPool, request_id: &str) -> i64 {
    let request_event_id: Uuid =
        sqlx::query_scalar("SELECT request_event_id FROM recovery_requests WHERE id = $1")
            .bind(Uuid::parse_str(request_id).unwrap())
            .fetch_one(pool)
            .await
            .unwrap();
    for _ in 0..300 {
        let covered: Option<OffsetDateTime> = sqlx::query_scalar(
            "SELECT h.created_at FROM signed_tree_heads h \
             JOIN ledger_entries e ON e.event_id = $1 AND h.tree_size >= e.seq \
             ORDER BY h.tree_size LIMIT 1",
        )
        .bind(request_event_id)
        .fetch_optional(pool)
        .await
        .unwrap();
        if let Some(covered) = covered {
            let size: i64 = sqlx::query_scalar(
                "INSERT INTO signed_tree_heads \
                 (tree_size, root_hash, network_id, signing_key_id, signature, created_at, \
                  layout_version, rules_version, hash_algo, extensions) \
                 SELECT tree_size + 100000, repeat('0', 64), network_id, signing_key_id, \
                        signature, $1, layout_version, rules_version, hash_algo, extensions \
                 FROM signed_tree_heads ORDER BY tree_size DESC LIMIT 1 RETURNING tree_size",
            )
            .bind(covered + time::Duration::hours(49))
            .fetch_one(pool)
            .await
            .unwrap();
            return size;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("no tree head ever covered the request marker");
}

async fn remove_head(pool: &PgPool, tree_size: i64) {
    sqlx::query("DELETE FROM signed_tree_heads WHERE tree_size = $1")
        .bind(tree_size)
        .execute(pool)
        .await
        .unwrap();
}

/// The `finalize` body: the recovered event of `GET .../recovered-draft`, signed by `signer`
/// (the request's new key unless a test passes another) at the owner's chain head plus one,
/// with the approval ids the draft carried.
async fn finalize_body(
    http: &reqwest::Client,
    base: &str,
    owner: avalon_protocol::ids::IdentityId,
    request_id: &str,
    signer: &SigningKey,
) -> serde_json::Value {
    let draft: serde_json::Value = http
        .get(format!(
            "{base}/recovery/requests/{request_id}/recovered-draft"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("recovered-draft should be available")
        .json()
        .await
        .unwrap();
    let (seq, prev) = chain_sign::head(owner).await;
    let chain_event = chain_sign::sign_at(
        signer,
        request_id.parse().unwrap(),
        draft["kind"].as_str().unwrap(),
        serde_json::from_value(draft["issuer"].clone()).unwrap(),
        serde_json::from_value(draft["subject"].clone()).unwrap(),
        draft["payload"].clone(),
        seq + 1,
        prev,
    )
    .await;
    serde_json::json!({ "chain_event": chain_event, "approval_ids": draft["approval_ids"] })
}

/// A request-shaped `chain_event` whose signature cannot verify, for requests refused before it is read.
fn unverifiable_chain_event() -> serde_json::Value {
    serde_json::json!({
        "event_id": Uuid::new_v4(),
        "timestamp": OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        "seq": 1,
        "prev_hash": null,
        "signing_key_id": Uuid::new_v4(),
        "signature": BASE64.encode([0u8; 64]),
    })
}

/// Configures `owner`'s guardian set to exactly `guardians` at `threshold`,
/// via the session-authenticated endpoint (never directly via SQL) — this
/// is itself part of what's under test: the config endpoint requires the
/// owner's current session. #697/#698: a fresh signature is always
/// supplied here since a from-empty first call always raises the
/// threshold above the unconfigured default of 0, putting it in the
/// signature-required case every time in practice.
async fn configure_guardians(
    http: &reqwest::Client,
    base: &str,
    owner_id: avalon_protocol::ids::IdentityId,
    owner_token: &str,
    guardians: &[avalon_protocol::ids::IdentityId],
    threshold: i32,
) {
    let (signing_key, signing_key_id) = chain_sign::key_of(owner_id);
    let mut sorted_guardians: Vec<avalon_protocol::ids::IdentityId> = guardians.to_vec();
    sorted_guardians.sort();
    sorted_guardians.dedup();
    let guardians_field = sorted_guardians
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let signature = sign_action(
        &signing_key,
        "recovery.guardians.set",
        &[
            &owner_id.to_string(),
            &guardians_field,
            &threshold.to_string(),
        ],
    );
    auth(
        http.put(format!("{base}/me/recovery/guardians")),
        owner_token,
    )
    .json(&serde_json::json!({
        "guardian_ids": guardians,
        "threshold": threshold,
        "signing_key_id": signing_key_id,
        "signature": signature,
        "chain_event": chain_sign::recovery_configured(owner_id, guardians.to_vec(), threshold).await,
    }))
    .send()
    .await
    .unwrap()
    .error_for_status()
    .expect("guardian configuration should succeed");
}

#[tokio::test]
#[ignore]
async fn three_guardian_two_of_three_threshold_recovers_after_backdated_delay() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, g1_token) = seed_identity_session(&pool).await;
    let (g2_id, _g2_token) = seed_identity_session(&pool).await;
    let (g3_id, g3_token) = seed_identity_session(&pool).await;
    for guardian in [g1_id, g2_id, g3_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }
    configure_guardians(
        &http,
        &base,
        owner_id,
        &owner_token,
        &[g1_id, g2_id, g3_id],
        2,
    )
    .await;

    let (request, new_key) = initiate_recovery(&http, &base, owner_id).await;
    assert_eq!(request["status"], "pending_approvals");
    let request_id = request["id"].as_str().unwrap().to_string();

    // A single approval never reaches the 2-of-3 threshold.
    let after_first = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .json(&approval(&http, &base, owner_id, g1_id, &request_id, &new_key).await)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json::<serde_json::Value>()
    .await
    .unwrap();
    assert_eq!(after_first["status"], "pending_approvals");
    assert_eq!(after_first["approvals_count"], 1);

    // Finalizing before threshold — and before any delay window even
    // exists — is refused.
    let too_early = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert_eq!(too_early.status().as_u16(), 409);

    // The second approval clears the threshold and opens the delay
    // window.
    let after_second = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g3_token,
    )
    .json(&approval(&http, &base, owner_id, g3_id, &request_id, &new_key).await)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json::<serde_json::Value>()
    .await
    .unwrap();
    assert_eq!(after_second["status"], "delay");
    assert_eq!(after_second["approvals_count"], 2);

    // Still within the delay window — finalize is refused.
    let still_too_early = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert_eq!(still_too_early.status().as_u16(), 409);

    // Let the delay run in cosigned-head time (see module docs) rather than sleeping for real
    // hours, then finalize should succeed.
    let request_uuid = Uuid::parse_str(&request_id).unwrap();
    let added_head = elapse_delay(&pool, &request_id).await;

    let me_status = |token: String| {
        let http = http.clone();
        let base = base.clone();
        async move {
            auth(http.get(format!("{base}/me")), &token)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(me_status(owner_token.clone()).await, 200);

    let stale_code = format!("stale-pairing-code-{}", Uuid::new_v4());
    // A pre-recovery passkey with a session it created, and an approved pairing nobody collected.
    let old_passkey: Uuid = sqlx::query_scalar(
        "INSERT INTO identity_keys (identity_id, credential_id, passkey_data) \
         VALUES ($1, decode(md5(random()::text), 'hex'), '{}'::jsonb) RETURNING id",
    )
    .bind(owner_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO indexer_identity_passkeys (passkey_id, identity_id, credential_id, passkey_data, added_at) \
         SELECT id, identity_id, credential_id, passkey_data, now() FROM identity_keys WHERE id = $1",
    )
    .bind(old_passkey)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO device_pairings (device_code_hash, user_code, status, identity_id, approved_by_signing_key_id, expires_at) \
         VALUES (encode(sha256(convert_to($3::text, 'UTF8')), 'hex'), $2, 'approved', $1, gen_random_uuid(), now() + interval '1 hour')",
    )
    .bind(owner_id)
    .bind(format!("T{}", &Uuid::new_v4().simple().to_string()[..7]))
    .bind(&stale_code)
    .execute(&pool)
    .await
    .unwrap();

    // A recovered event signed by any key other than the request's new key is refused untouched.
    let wrong_signer = SigningKey::generate(&mut rand::rng());
    let wrong = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &wrong_signer).await)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 400);
    let wrong_body: serde_json::Value = wrong.json().await.unwrap();
    assert_eq!(wrong_body["code"], "INVALID_AUTHOR_SIGNATURE");
    let status_after_wrong: String =
        sqlx::query_scalar("SELECT status FROM recovery_requests WHERE id = $1")
            .bind(request_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status_after_wrong, "delay");

    let finalized = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("finalize should succeed once the delay has elapsed")
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(finalized["status"], "completed");

    // Completing recovery ends every session the identity held before it, and only those.
    assert_eq!(me_status(owner_token.clone()).await, 401);
    assert_eq!(me_status(g1_token.clone()).await, 200);

    // Old credentials are gone and revoked durably; the stale approved pairing cannot be collected.
    let old_still_there: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM identity_keys WHERE id = $1")
            .bind(old_passkey)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(old_still_there, 0);
    let revoked_durably: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM indexer_identity_passkeys \
         WHERE identity_id = $1 AND passkey_id = $2 AND revoked_at IS NOT NULL",
    )
    .bind(owner_id)
    .bind(old_passkey)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(revoked_durably, 1);
    let active_keys: Vec<Uuid> = sqlx::query_scalar(
        "SELECT signing_key_id FROM indexer_identity_signing_keys \
         WHERE identity_id = $1 AND revoked_at IS NULL",
    )
    .bind(owner_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(active_keys, vec![request_uuid]);
    let poll = http
        .post(format!("{base}/auth/device/poll"))
        .bearer_auth(&stale_code)
        .send()
        .await
        .unwrap();
    assert_eq!(poll.status().as_u16(), 401);

    // The new device's passkey is now a real, ordinary login credential.
    let passkey_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM identity_keys WHERE identity_id = $1")
            .bind(owner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(passkey_count, 1);

    // Finalizing again is a harmless idempotent no-op, not an error.
    let refinalized = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert!(refinalized.status().is_success());
    remove_head(&pool, added_head).await;
    let passkey_count_after_refinalize: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM identity_keys WHERE identity_id = $1")
            .bind(owner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(passkey_count_after_refinalize, 1);
}

#[tokio::test]
#[ignore]
async fn a_guardian_below_threshold_cannot_unilaterally_recover() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, g1_token) = seed_identity_session(&pool).await;
    let (g2_id, _g2_token) = seed_identity_session(&pool).await;
    for guardian in [g1_id, g2_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }
    configure_guardians(&http, &base, owner_id, &owner_token, &[g1_id, g2_id], 2).await;

    let (request, new_key) = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    let after_one = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .json(&approval(&http, &base, owner_id, g1_id, &request_id, &new_key).await)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json::<serde_json::Value>()
    .await
    .unwrap();
    assert_eq!(after_one["status"], "pending_approvals");

    // Even a delay that has run can't help — the request never left `pending_approvals`, so
    // finalize must still refuse.
    let request_uuid = Uuid::parse_str(&request_id).unwrap();
    let added_head = elapse_delay(&pool, &request_id).await;

    let finalize_attempt = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert_eq!(finalize_attempt.status().as_u16(), 409);

    // Even forced into the delay phase with one approval, finalize is refused and changes nothing.
    sqlx::query("UPDATE recovery_requests SET status = 'delay' WHERE id = $1")
        .bind(request_uuid)
        .execute(&pool)
        .await
        .unwrap();
    let forced = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert!(!forced.status().is_success(), "{:?}", forced.status());
    remove_head(&pool, added_head).await;
    let status: String = sqlx::query_scalar("SELECT status FROM recovery_requests WHERE id = $1")
        .bind(request_uuid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "delay");

    let passkey_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM identity_keys WHERE identity_id = $1")
            .bind(owner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(passkey_count, 0);
}

#[tokio::test]
#[ignore]
async fn the_owner_vetoes_and_the_attempt_is_cancelled() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, g1_token) = seed_identity_session(&pool).await;
    let (g2_id, g2_token) = seed_identity_session(&pool).await;
    for guardian in [g1_id, g2_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }
    configure_guardians(&http, &base, owner_id, &owner_token, &[g1_id, g2_id], 2).await;

    let (request, new_key) = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .json(&approval(&http, &base, owner_id, g1_id, &request_id, &new_key).await)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    let after_second = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g2_token,
    )
    .json(&approval(&http, &base, owner_id, g2_id, &request_id, &new_key).await)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json::<serde_json::Value>()
    .await
    .unwrap();
    assert_eq!(after_second["status"], "delay");

    // The real owner still has a working session (this test's whole
    // premise — a still-lost-every-device owner obviously can't hit this
    // endpoint, but that's exactly why the delay/public-visibility path
    // exists for that harder case) and vetoes.
    let cancelled = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/cancel")),
        &owner_token,
    )
    .json(&cancel_body(owner_id, &request_id, Some("not me")).await)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .expect("cancel should succeed")
    .json::<serde_json::Value>()
    .await
    .unwrap();
    assert_eq!(cancelled["status"], "cancelled");

    // Even a delay that has run after a cancel can never resurrect it.
    let added_head = elapse_delay(&pool, &request_id).await;
    let finalize_after_cancel = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert_eq!(finalize_after_cancel.status().as_u16(), 409);
    remove_head(&pool, added_head).await;

    let passkey_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM identity_keys WHERE identity_id = $1")
            .bind(owner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(passkey_count, 0);

    // A fresh recovery attempt against the same identity is unaffected —
    // the one-active-attempt constraint only ever blocks *concurrent*
    // attempts, never future ones once the prior attempt resolved.
    let (second_attempt, _) = initiate_recovery(&http, &base, owner_id).await;
    assert_eq!(second_attempt["status"], "pending_approvals");
}

#[tokio::test]
#[ignore]
async fn a_guardian_removed_from_the_set_can_no_longer_approve_or_cancel() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, g1_token) = seed_identity_session(&pool).await;
    let (g2_id, _g2_token) = seed_identity_session(&pool).await;
    let (g3_id, _g3_token) = seed_identity_session(&pool).await;
    for guardian in [g1_id, g2_id, g3_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }
    configure_guardians(
        &http,
        &base,
        owner_id,
        &owner_token,
        &[g1_id, g2_id, g3_id],
        2,
    )
    .await;

    // Owner narrows the guardian set to g2 and g3 — requires (and proves)
    // the owner's own current session, never g1's cooperation.
    configure_guardians(&http, &base, owner_id, &owner_token, &[g2_id, g3_id], 2).await;

    let (request, new_key) = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    let approve_as_removed_guardian = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .json(&approval(&http, &base, owner_id, g1_id, &request_id, &new_key).await)
    .send()
    .await
    .unwrap();
    assert_eq!(approve_as_removed_guardian.status().as_u16(), 403);

    // Only the owner may cancel; a guardian's recourse is to withdraw their own approval.
    let cancel_as_guardian = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/cancel")),
        &g1_token,
    )
    .json(&cancel_body(g1_id, &request_id, None).await)
    .send()
    .await
    .unwrap();
    assert_eq!(cancel_as_guardian.status().as_u16(), 403);
}

#[tokio::test]
#[ignore]
async fn changing_the_guardian_set_requires_a_valid_session() {
    let http = reqwest::Client::new();
    let base = server_url();

    let unauthenticated = http
        .put(format!("{base}/me/recovery/guardians"))
        .json(&serde_json::json!({
            "guardian_ids": [],
            "threshold": 1,
            "chain_event": unverifiable_chain_event(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status().as_u16(), 401);
}

/// #697/#698: removing an existing guardian is one of the two conditions
/// that puts this write in the signature-required tier — an unsigned
/// removal attempt must be rejected and the guardian set left unchanged; a
/// correctly signed one must go through.
#[tokio::test]
#[ignore]
async fn removing_a_guardian_requires_a_fresh_signature() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, _g1_token) = seed_identity_session(&pool).await;
    let (g2_id, _g2_token) = seed_identity_session(&pool).await;
    let (g3_id, _g3_token) = seed_identity_session(&pool).await;
    for guardian in [g1_id, g2_id, g3_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }

    configure_guardians(
        &http,
        &base,
        owner_id,
        &owner_token,
        &[g1_id, g2_id, g3_id],
        2,
    )
    .await;

    // Dropping g1 without a signature is rejected outright.
    let unsigned = auth(
        http.put(format!("{base}/me/recovery/guardians")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "guardian_ids": [g2_id, g3_id],
        "threshold": 2,
        "chain_event": chain_sign::recovery_configured(owner_id, vec![g2_id, g3_id], 2).await,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(unsigned.status().as_u16(), 401);
    let unsigned_body: serde_json::Value = unsigned.json().await.unwrap();
    assert_eq!(unsigned_body["code"], "FRESH_SIGNATURE_REQUIRED");

    let after_unsigned_attempt: serde_json::Value = auth(
        http.get(format!("{base}/me/recovery/guardians")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let guardian_ids: Vec<String> = after_unsigned_attempt["guardian_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(guardian_ids.len(), 3, "unsigned removal must not apply");

    // The same removal, correctly signed, succeeds.
    configure_guardians(&http, &base, owner_id, &owner_token, &[g2_id, g3_id], 2).await;

    let after_signed: serde_json::Value = auth(
        http.get(format!("{base}/me/recovery/guardians")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(after_signed["guardian_ids"].as_array().unwrap().len(), 2);
}

#[tokio::test]
#[ignore]
async fn guardians_must_be_current_friends_not_arbitrary_identities() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (stranger_id, _stranger_token) = seed_identity_session(&pool).await;
    let (other_stranger_id, _other_token) = seed_identity_session(&pool).await;

    let attempt = auth(
        http.put(format!("{base}/me/recovery/guardians")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "guardian_ids": [stranger_id, other_stranger_id],
        "threshold": 2,
        "chain_event": chain_sign::recovery_configured(
            owner_id,
            vec![stranger_id, other_stranger_id],
            2,
        )
        .await,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(attempt.status().as_u16(), 400);
}

/// Regression test for a real enumeration oracle found in review:
/// `POST /recovery/requests/start` must give an unauthenticated caller no
/// way to distinguish "this identity doesn't exist" from "this identity
/// exists but has no guardians configured" — same status, same body, for
/// both cases.
#[tokio::test]
#[ignore]
async fn start_gives_no_distinguishable_signal_between_nonexistent_and_unconfigured_identity() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (configured_owner, configured_token) = seed_identity_session(&pool).await;
    let (guardian_id, _guardian_token) = seed_identity_session(&pool).await;
    let (second_guardian_id, _second_guardian_token) = seed_identity_session(&pool).await;
    seed_friendship(&pool, configured_owner, guardian_id).await;
    seed_friendship(&pool, configured_owner, second_guardian_id).await;
    configure_guardians(
        &http,
        &base,
        configured_owner,
        &configured_token,
        &[guardian_id, second_guardian_id],
        2,
    )
    .await;
    // Deliberately NOT calling configure_guardians for a second, real
    // identity — that one stays "exists but unconfigured".
    let (unconfigured_owner, _unconfigured_token) = seed_identity_session(&pool).await;

    let nonexistent_id = avalon_protocol::ids::IdentityId::random_for_tests();

    let start_request = |identity_id: avalon_protocol::ids::IdentityId| {
        let http = &http;
        let base = &base;
        async move {
            http.post(format!("{base}/recovery/requests/start"))
                .json(&serde_json::json!({ "identity_id": identity_id, "device_label": "probe" }))
                .send()
                .await
                .unwrap()
        }
    };

    let against_nonexistent = start_request(nonexistent_id).await;
    let nonexistent_status = against_nonexistent.status().as_u16();
    let nonexistent_body: serde_json::Value = against_nonexistent.json().await.unwrap();

    let against_unconfigured = start_request(unconfigured_owner).await;
    let unconfigured_status = against_unconfigured.status().as_u16();
    let unconfigured_body: serde_json::Value = against_unconfigured.json().await.unwrap();

    assert_eq!(
        nonexistent_status, unconfigured_status,
        "a nonexistent identity and an unconfigured-but-real one must return the same status"
    );
    assert_eq!(
        nonexistent_body, unconfigured_body,
        "a nonexistent identity and an unconfigured-but-real one must return the same body — \
         any difference is an oracle for enumerating which identity ids are real"
    );
    // Sanity: a *configured* identity behaves differently (returns a real
    // challenge, not an error), so this test would fail if the endpoint
    // stopped distinguishing configured-vs-not at all.
    let against_configured = start_request(configured_owner).await;
    assert!(against_configured.status().is_success());
}

/// Regression test for a real race condition found in review: a guardian
/// approval racing a concurrent owner/guardian cancellation must never
/// silently resurrect a cancelled request. Fires both requests
/// concurrently (real async tasks, not sequential awaits) — Postgres row
/// locking (`SELECT ... FOR UPDATE` in `approve_request`/`cancel_request`)
/// is what actually guarantees the invariant below regardless of which
/// request's transaction commits first, not test scheduling luck.
#[tokio::test]
#[ignore]
async fn a_racing_approval_never_resurrects_a_cancelled_request() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, g1_token) = seed_identity_session(&pool).await;
    let (g2_id, g2_token) = seed_identity_session(&pool).await;
    for guardian in [g1_id, g2_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }
    configure_guardians(&http, &base, owner_id, &owner_token, &[g1_id, g2_id], 2).await;

    let (request, new_key) = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    // First guardian approves normally, so the request is one approval
    // away from crossing the threshold.
    auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .json(&approval(&http, &base, owner_id, g1_id, &request_id, &new_key).await)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();

    // Now race the owner's veto against the second guardian's approval —
    // whichever the database actually serializes first is authoritative;
    // what must never happen is the row ending up with `cancelled_at` set
    // while `status` says anything other than `cancelled`.
    let cancel_request = cancel_body(owner_id, &request_id, None).await;
    let cancel = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/cancel")),
        &owner_token,
    )
    .json(&cancel_request)
    .send();
    let approve = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g2_token,
    )
    .json(&approval(&http, &base, owner_id, g2_id, &request_id, &new_key).await)
    .send();
    let (_cancel_result, _approve_result) = tokio::join!(cancel, approve);

    let row = sqlx::query("SELECT status, cancelled_at FROM recovery_requests WHERE id = $1")
        .bind(Uuid::parse_str(&request_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    let status: String = sqlx::Row::try_get(&row, "status").unwrap();
    let cancelled_at: Option<OffsetDateTime> = sqlx::Row::try_get(&row, "cancelled_at").unwrap();

    if cancelled_at.is_some() {
        assert_eq!(
            status, "cancelled",
            "a request with cancelled_at set must have status = 'cancelled', never \
             silently overwritten back to an active state by a racing approval"
        );
    }

    // Whatever the final state, finalize must never succeed for a request
    // whose status isn't a genuine, uncancelled 'delay' past its window —
    // regardless of which way the race resolved.
    let finalize = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    if status == "cancelled" {
        assert!(
            !finalize.status().is_success(),
            "finalize must never succeed for a cancelled request"
        );
    }
}

/// Issue #443: a guardian can discover every identity that currently names
/// them, and only those identities.
#[tokio::test]
#[ignore]
async fn guardian_of_lists_only_identities_naming_the_caller() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (guardian_id, guardian_token) = seed_identity_session(&pool).await;
    let (bystander_id, bystander_token) = seed_identity_session(&pool).await;
    let _ = bystander_id;

    let (second_id, _second_token) = seed_identity_session(&pool).await;
    seed_friendship(&pool, owner_id, guardian_id).await;
    seed_friendship(&pool, owner_id, second_id).await;
    configure_guardians(
        &http,
        &base,
        owner_id,
        &owner_token,
        &[guardian_id, second_id],
        2,
    )
    .await;

    let mine: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/me/recovery/guardian-of")),
        &guardian_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(
        mine[0]["identity_id"].as_str().unwrap(),
        owner_id.to_string()
    );

    let bystanders: Vec<serde_json::Value> = auth(
        http.get(format!("{base}/me/recovery/guardian-of")),
        &bystander_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(bystanders.is_empty());
}

/// Issue #443: a guardian can resign without the owner's cooperation, and
/// only from their own designation — never on behalf of another guardian.
#[tokio::test]
#[ignore]
async fn a_guardian_can_resign_without_the_owners_cooperation() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, g1_token) = seed_identity_session(&pool).await;
    let (g2_id, g2_token) = seed_identity_session(&pool).await;

    seed_friendship(&pool, owner_id, g1_id).await;
    seed_friendship(&pool, owner_id, g2_id).await;
    configure_guardians(&http, &base, owner_id, &owner_token, &[g1_id, g2_id], 2).await;

    // A guardian resigning from a designation they don't hold is a no-op
    // failure, not a way to remove someone else.
    let wrong_target = auth(
        http.delete(format!("{base}/me/recovery/guardian-of/{g1_id}")),
        &g2_token,
    )
    .send()
    .await
    .unwrap();
    assert!(!wrong_target.status().is_success());

    let resign = auth(
        http.delete(format!("{base}/me/recovery/guardian-of/{owner_id}")),
        &g1_token,
    )
    .send()
    .await
    .unwrap();
    assert!(resign.status().is_success(), "{:?}", resign.status());

    let settings: serde_json::Value = auth(
        http.get(format!("{base}/me/recovery/guardians")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let remaining_guardians = settings["guardian_ids"].as_array().unwrap();
    assert_eq!(remaining_guardians.len(), 1);
    assert_eq!(remaining_guardians[0].as_str().unwrap(), g2_id.to_string());
    // Threshold was 2 against 2 guardians; dropping to 1 guardian must clamp
    // the threshold down rather than leave an unsatisfiable 2-of-1 config.
    assert_eq!(settings["threshold"].as_i64().unwrap(), 1);
}

/// An owner with two seeded passkeys and a recovery request whose delay has elapsed.
async fn recovery_ready_to_finalize(
    pool: &PgPool,
    http: &reqwest::Client,
    base: &str,
) -> (
    avalon_protocol::ids::IdentityId,
    String,
    String,
    Vec<Uuid>,
    SigningKey,
    i64,
) {
    let (owner_id, owner_token) = seed_identity_session(pool).await;
    let (g1_id, g1_token) = seed_identity_session(pool).await;
    let (g2_id, _g2_token) = seed_identity_session(pool).await;
    let (g3_id, g3_token) = seed_identity_session(pool).await;
    for guardian in [g1_id, g2_id, g3_id] {
        seed_friendship(pool, owner_id, guardian).await;
    }
    configure_guardians(
        http,
        base,
        owner_id,
        &owner_token,
        &[g1_id, g2_id, g3_id],
        2,
    )
    .await;
    let mut old_passkeys = Vec::new();
    for _ in 0..2 {
        old_passkeys.push(
            sqlx::query_scalar(
                "INSERT INTO identity_keys (identity_id, credential_id, passkey_data) \
                 VALUES ($1, decode(md5(random()::text), 'hex'), '{}'::jsonb) RETURNING id",
            )
            .bind(owner_id)
            .fetch_one(pool)
            .await
            .unwrap(),
        );
    }
    let (request, new_key) = initiate_recovery(http, base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();
    for (guardian, token) in [(g1_id, &g1_token), (g3_id, &g3_token)] {
        auth(
            http.post(format!("{base}/recovery/requests/{request_id}/approve")),
            token,
        )
        .json(&approval(http, base, owner_id, guardian, &request_id, &new_key).await)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    }
    let added_head = elapse_delay(pool, &request_id).await;
    (
        owner_id,
        owner_token,
        request_id,
        old_passkeys,
        new_key,
        added_head,
    )
}

async fn passkey_revoked_event_count(
    pool: &PgPool,
    table_sql: &'static str,
    owner_id: avalon_protocol::ids::IdentityId,
    passkey_id: Uuid,
) -> i64 {
    sqlx::query_scalar(table_sql)
        .bind(owner_id.to_string())
        .bind(passkey_id.to_string())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn finalize_revokes_old_credentials_without_per_passkey_events() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (owner_id, _token, request_id, old_passkeys, new_key, added_head) =
        recovery_ready_to_finalize(&pool, &http, &base).await;

    http.post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    for passkey_id in old_passkeys {
        let emitted = passkey_revoked_event_count(
            &pool,
            "SELECT COUNT(*) FROM protocol_outbox WHERE event->>'kind' = 'identity.passkey_revoked' \
             AND event->'payload'->>'identity_id' = $1 AND event->'payload'->>'passkey_id' = $2",
            owner_id,
            passkey_id,
        )
        .await;
        assert_eq!(emitted, 0, "passkey {passkey_id}");
    }
    let recovered: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM identity_chain_events WHERE identity_id = $1 \
         AND event->>'kind' = 'identity.recovered'",
    )
    .bind(owner_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    remove_head(&pool, added_head).await;
}

type MirroredPasskey = (Uuid, Vec<u8>, serde_json::Value, Option<String>);

/// The identity's active passkeys as a mirror derives them: its emitted events replayed in
/// order through the production projections into empty temp copies of the mirror tables.
async fn replayed_active_passkeys(
    pool: &PgPool,
    owner: avalon_protocol::ids::IdentityId,
) -> Vec<MirroredPasskey> {
    let mut tx = pool.begin().await.unwrap();
    for create in [
        "CREATE TEMP TABLE indexer_identity_passkeys (LIKE indexer_identity_passkeys INCLUDING ALL) ON COMMIT DROP",
        "CREATE TEMP TABLE indexer_identity_passkey_revocations (LIKE indexer_identity_passkey_revocations INCLUDING ALL) ON COMMIT DROP",
        "CREATE TEMP TABLE indexer_identity_signing_keys (LIKE indexer_identity_signing_keys INCLUDING ALL) ON COMMIT DROP",
    ] {
        sqlx::query(create).execute(&mut *tx).await.unwrap();
    }
    let events: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT event FROM protocol_outbox WHERE event->>'issuer' LIKE $1 ORDER BY seq",
    )
    .bind(format!("identity:{owner}:%"))
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    for raw in events {
        let event: avalon_protocol::events::ProtocolEvent = serde_json::from_value(raw).unwrap();
        match event.kind.as_str() {
            "identity.recovered" => {
                avalon_indexer::projections::identity_signing_keys::apply_recovered(
                    &mut tx, &event, owner,
                )
                .await
                .unwrap()
            }
            _ => {
                if let Some(write) = avalon_indexer::projections::identity_passkeys::decode(&event)
                {
                    avalon_indexer::projections::identity_passkeys::apply(&mut tx, &write)
                        .await
                        .unwrap();
                }
            }
        }
    }
    sqlx::query_as(
        "SELECT passkey_id, credential_id, passkey_data, label FROM indexer_identity_passkeys \
         WHERE identity_id = $1 AND revoked_at IS NULL",
    )
    .bind(owner)
    .fetch_all(&mut *tx)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn after_recovery_a_mirror_sees_exactly_the_announced_replacement_passkey() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (owner_id, _token, request_id, old_passkeys, new_key, added_head) =
        recovery_ready_to_finalize(&pool, &http, &base).await;
    http.post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    remove_head(&pool, added_head).await;

    let (passkey_id, credential_id, passkey_data, label, announced_at): (
        Uuid,
        Vec<u8>,
        serde_json::Value,
        Option<String>,
        Option<OffsetDateTime>,
    ) = sqlx::query_as(
        "SELECT id, credential_id, passkey_data, label, announced_at FROM identity_keys \
         WHERE identity_id = $1",
    )
    .bind(owner_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!old_passkeys.contains(&passkey_id));
    assert!(announced_at.is_none());
    assert!(replayed_active_passkeys(&pool, owner_id).await.is_empty());

    // The owner holds a session from the replacement passkey and signs with the recovery key.
    let token = format!("test-token-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO sessions (token_hash, identity_id, expires_at) VALUES (sha256(convert_to($1::text, 'UTF8')), $2, now() + interval '1 hour')")
        .bind(&token)
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
    chain_sign::remember(owner_id, new_key, request_id.parse().unwrap());
    let announced = chain_sign::announce_passkey(&http, &token, owner_id, passkey_id).await;
    assert!(announced.status().is_success(), "{:?}", announced.status());

    let active = replayed_active_passkeys(&pool, owner_id).await;
    assert_eq!(
        active,
        vec![(passkey_id, credential_id, passkey_data, label)]
    );
}

#[tokio::test]
#[ignore]
async fn a_finalize_that_fails_after_the_passkey_delete_changes_nothing() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (owner_id, owner_token, request_id, old_passkeys, new_key, added_head) =
        recovery_ready_to_finalize(&pool, &http, &base).await;

    // Another identity already holds the credential id the recovered passkey would insert.
    let (other_id, _) = seed_identity_session(&pool).await;
    sqlx::query(
        "INSERT INTO identity_keys (identity_id, credential_id, passkey_data) \
         SELECT $1, pending_credential_id, '{}'::jsonb FROM recovery_requests WHERE id = $2",
    )
    .bind(other_id)
    .bind(Uuid::parse_str(&request_id).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    let finalize = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert!(finalize.status().is_server_error() || finalize.status().is_client_error());

    let remaining: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM identity_keys WHERE identity_id = $1 ORDER BY id")
            .bind(owner_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut expected = old_passkeys.clone();
    expected.sort();
    assert_eq!(remaining, expected);
    let status: String = sqlx::query_scalar("SELECT status FROM recovery_requests WHERE id = $1")
        .bind(Uuid::parse_str(&request_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "delay");
    let revocations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM indexer_identity_passkey_revocations WHERE identity_id = $1",
    )
    .bind(owner_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(revocations, 0);
    let me = auth(http.get(format!("{base}/me")), &owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status().as_u16(), 200);
    remove_head(&pool, added_head).await;
}

/// The recovered draft of a request.
async fn draft(http: &reqwest::Client, base: &str, request_id: &str) -> serde_json::Value {
    http.get(format!(
        "{base}/recovery/requests/{request_id}/recovered-draft"
    ))
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn a_withdrawal_rides_in_the_proof_and_a_stale_draft_is_answered_with_the_current_hash() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let mut guardians = Vec::new();
    for _ in 0..3 {
        guardians.push(seed_identity_session(&pool).await);
    }
    for (guardian, _) in &guardians {
        seed_friendship(&pool, owner_id, *guardian).await;
    }
    let ids: Vec<_> = guardians.iter().map(|(id, _)| *id).collect();
    configure_guardians(&http, &base, owner_id, &owner_token, &ids, 2).await;
    let (request, new_key) = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();
    let approve = |i: usize| {
        let (http, base, request_id, new_key) = (&http, &base, &request_id, &new_key);
        let (guardian, token) = guardians[i].clone();
        async move {
            auth(
                http.post(format!("{base}/recovery/requests/{request_id}/approve")),
                &token,
            )
            .json(&approval(http, base, owner_id, guardian, request_id, new_key).await)
            .send()
            .await
            .unwrap()
        }
    };
    let withdraw = |i: usize| {
        let (http, base, request_id) = (&http, &base, &request_id);
        let (guardian, token) = guardians[i].clone();
        async move {
            auth(
                http.post(format!("{base}/recovery/requests/{request_id}/withdraw")),
                &token,
            )
            .json(&withdrawal(owner_id, guardian, request_id).await)
            .send()
            .await
            .unwrap()
        }
    };

    assert!(approve(0).await.status().is_success());
    let second = approve(2).await.json::<serde_json::Value>().await.unwrap();
    assert_eq!(second["status"], "delay");
    let stale_body = finalize_body(&http, &base, owner_id, &request_id, &new_key).await;

    // A third approval changes the draft: the draft signed earlier is answered with the current hash.
    assert!(approve(1).await.status().is_success());
    let added_head = elapse_delay(&pool, &request_id).await;
    let stale = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&stale_body)
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status().as_u16(), 409);
    let stale: serde_json::Value = stale.json().await.unwrap();
    assert_eq!(stale["code"], "RECOVERY_DRAFT_STALE");
    let current = draft(&http, &base, &request_id).await;
    assert_eq!(stale["draft_hash"], current["draft_hash"]);

    // Withdrawing a standing approval is allowed once, and it rides inside the approval proof.
    let withdrawn = withdraw(0).await;
    assert!(withdrawn.status().is_success(), "{:?}", withdrawn.status());
    let withdrawn: serde_json::Value = withdrawn.json().await.unwrap();
    assert_eq!(withdrawn["approvals_count"], 2);
    assert_eq!(withdrawn["status"], "delay");
    assert_eq!(withdraw(0).await.status().as_u16(), 404);
    let proofs = draft(&http, &base, &request_id).await["payload"]["approvals"].clone();
    let with_withdrawal: Vec<_> = proofs
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| !p["withdrawal"].is_null())
        .collect();
    assert_eq!(with_withdrawal.len(), 1);

    // Below the threshold the request goes back to collecting approvals and cannot finalize.
    let dropped = withdraw(1).await.json::<serde_json::Value>().await.unwrap();
    assert_eq!(dropped["status"], "pending_approvals");
    assert_eq!(dropped["approvals_count"], 1);
    let blocked = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .json(&finalize_body(&http, &base, owner_id, &request_id, &new_key).await)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status().as_u16(), 409);
    remove_head(&pool, added_head).await;

    // A guardian who withdrew can approve again, which replaces the withdrawn approval.
    let again = approve(1).await.json::<serde_json::Value>().await.unwrap();
    assert_eq!(again["approvals_count"], 2);
    assert_eq!(again["status"], "delay");
}

#[tokio::test]
#[ignore]
async fn a_threshold_below_the_protocol_minimum_is_refused() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (g1_id, _) = seed_identity_session(&pool).await;
    let (g2_id, _) = seed_identity_session(&pool).await;
    for guardian in [g1_id, g2_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }
    let attempt = auth(
        http.put(format!("{base}/me/recovery/guardians")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "guardian_ids": [g1_id, g2_id],
        "threshold": 1,
        "chain_event": chain_sign::recovery_configured(owner_id, vec![g1_id, g2_id], 1).await,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(attempt.status().as_u16(), 400);
}
