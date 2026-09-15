//! Exercises the social-recovery flow (issue #201) against a real, running
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
//! The mandatory public time-delay is not waited out in real time — these
//! tests instead assert the *state machine* (status transitions,
//! `delay_ends_at` being set correctly, finalize refusing early) and, for
//! the one test that exercises a successful finalize end-to-end, backdate
//! `delay_ends_at` directly via SQL rather than sleeping for
//! `AVALON_RECOVERY_DELAY_HOURS` real hours — the same "manipulate the
//! clock-dependent column directly, since the point under test is the
//! comparison logic, not the wall-clock wait" approach `presence.rs`'s
//! `stale_presence_expires_to_offline` uses for TTL expiry.

use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

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

type VirtualClient =
    Client<MemoryStore, MockUserValidationMethod, public_suffix::PublicSuffixList, ()>;

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
    );
    Client::new(authenticator).allows_insecure_localhost(true)
}

/// Seeds a bare identity + profile + session, bypassing WebAuthn entirely
/// — same approach `friends.rs::seed_identity_session` takes. Used for the
/// owner and every guardian, none of which need a real passkey for these
/// tests (only the *recovering device* does, since it's the one thing the
/// server genuinely runs a WebAuthn ceremony for).
async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("recovery-test-{identity_id}"))
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

async fn seed_friendship(pool: &PgPool, a: Uuid, b: Uuid) {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    sqlx::query("INSERT INTO friendships (a, b) VALUES ($1, $2)")
        .bind(lo)
        .bind(hi)
        .execute(pool)
        .await
        .expect("failed to seed friendship");
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

/// Drives a real recovery-initiation ceremony (`start`/`finish`) for
/// `identity_id` and returns the resulting `recovery_requests` row id plus
/// its JSON body.
async fn initiate_recovery(
    http: &reqwest::Client,
    base: &str,
    identity_id: Uuid,
) -> serde_json::Value {
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

    http.post(format!("{base}/recovery/requests/finish"))
        .json(&serde_json::json!({
            "ticket_id": ticket_id,
            "webauthn_credential": credential,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("recovery/requests/finish should succeed")
        .json()
        .await
        .unwrap()
}

/// Configures `owner`'s guardian set to exactly `guardians` at `threshold`,
/// via the session-authenticated endpoint (never directly via SQL) — this
/// is itself part of what's under test: the config endpoint requires the
/// owner's current session.
async fn configure_guardians(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    guardians: &[Uuid],
    threshold: i32,
) {
    auth(
        http.put(format!("{base}/me/recovery/guardians")),
        owner_token,
    )
    .json(&serde_json::json!({ "guardian_ids": guardians, "threshold": threshold }))
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
    configure_guardians(&http, &base, &owner_token, &[g1_id, g2_id, g3_id], 2).await;

    let request = initiate_recovery(&http, &base, owner_id).await;
    assert_eq!(request["status"], "pending_approvals");
    let request_id = request["id"].as_str().unwrap().to_string();

    // A single approval never reaches the 2-of-3 threshold.
    let after_first = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
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
    assert!(!after_second["delay_ends_at"].is_null());

    // Still within the delay window — finalize is refused.
    let still_too_early = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .send()
        .await
        .unwrap();
    assert_eq!(still_too_early.status().as_u16(), 409);

    // Backdate the delay directly (see module docs) rather than sleeping
    // for real hours, then finalize should succeed.
    let request_uuid = Uuid::parse_str(&request_id).unwrap();
    sqlx::query(
        "UPDATE recovery_requests SET delay_ends_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(request_uuid)
    .execute(&pool)
    .await
    .unwrap();

    let finalized = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("finalize should succeed once the delay has elapsed")
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(finalized["status"], "completed");

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
        .send()
        .await
        .unwrap();
    assert!(refinalized.status().is_success());
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
    configure_guardians(&http, &base, &owner_token, &[g1_id, g2_id], 2).await;

    let request = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    let after_one = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json::<serde_json::Value>()
    .await
    .unwrap();
    assert_eq!(after_one["status"], "pending_approvals");

    // Even backdating a (nonexistent) delay can't help — the request never
    // left `pending_approvals`, so finalize must still refuse.
    let request_uuid = Uuid::parse_str(&request_id).unwrap();
    sqlx::query(
        "UPDATE recovery_requests SET delay_ends_at = now() - interval '1 hour' WHERE id = $1",
    )
    .bind(request_uuid)
    .execute(&pool)
    .await
    .unwrap();

    let finalize_attempt = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .send()
        .await
        .unwrap();
    assert_eq!(finalize_attempt.status().as_u16(), 409);

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
    configure_guardians(&http, &base, &owner_token, &[g1_id, g2_id], 2).await;

    let request = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    let after_second = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g2_token,
    )
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
    .json(&serde_json::json!({ "reason": "not me" }))
    .send()
    .await
    .unwrap()
    .error_for_status()
    .expect("cancel should succeed")
    .json::<serde_json::Value>()
    .await
    .unwrap();
    assert_eq!(cancelled["status"], "cancelled");

    // Even backdating the delay after a cancel can never resurrect it.
    let request_uuid = Uuid::parse_str(&request_id).unwrap();
    sqlx::query(
        "UPDATE recovery_requests SET delay_ends_at = now() - interval '1 hour' WHERE id = $1",
    )
    .bind(request_uuid)
    .execute(&pool)
    .await
    .unwrap();
    let finalize_after_cancel = http
        .post(format!("{base}/recovery/requests/{request_id}/finalize"))
        .send()
        .await
        .unwrap();
    assert_eq!(finalize_after_cancel.status().as_u16(), 409);

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
    let second_attempt = initiate_recovery(&http, &base, owner_id).await;
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
    for guardian in [g1_id, g2_id] {
        seed_friendship(&pool, owner_id, guardian).await;
    }
    configure_guardians(&http, &base, &owner_token, &[g1_id, g2_id], 2).await;

    // Owner narrows the guardian set to just g2 — requires (and proves)
    // the owner's own current session, never g1's cooperation.
    configure_guardians(&http, &base, &owner_token, &[g2_id], 1).await;

    let request = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    let approve_as_removed_guardian = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(approve_as_removed_guardian.status().as_u16(), 403);

    let cancel_as_removed_guardian = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/cancel")),
        &g1_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(cancel_as_removed_guardian.status().as_u16(), 403);
}

#[tokio::test]
#[ignore]
async fn changing_the_guardian_set_requires_a_valid_session() {
    let http = reqwest::Client::new();
    let base = server_url();

    let unauthenticated = http
        .put(format!("{base}/me/recovery/guardians"))
        .json(&serde_json::json!({ "guardian_ids": [], "threshold": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status().as_u16(), 401);
}

#[tokio::test]
#[ignore]
async fn guardians_must_be_current_friends_not_arbitrary_identities() {
    let pool = test_pool().await;
    let http = reqwest::Client::new();
    let base = server_url();

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (stranger_id, _stranger_token) = seed_identity_session(&pool).await;
    let _ = owner_id;

    let attempt = auth(
        http.put(format!("{base}/me/recovery/guardians")),
        &owner_token,
    )
    .json(&serde_json::json!({ "guardian_ids": [stranger_id], "threshold": 1 }))
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
    seed_friendship(&pool, configured_owner, guardian_id).await;
    configure_guardians(&http, &base, &configured_token, &[guardian_id], 1).await;
    // Deliberately NOT calling configure_guardians for a second, real
    // identity — that one stays "exists but unconfigured".
    let (unconfigured_owner, _unconfigured_token) = seed_identity_session(&pool).await;

    let nonexistent_id = Uuid::new_v4();

    let start_request = |identity_id: Uuid| {
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
    configure_guardians(&http, &base, &owner_token, &[g1_id, g2_id], 2).await;

    let request = initiate_recovery(&http, &base, owner_id).await;
    let request_id = request["id"].as_str().unwrap().to_string();

    // First guardian approves normally, so the request is one approval
    // away from crossing the threshold.
    auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g1_token,
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();

    // Now race the owner's veto against the second guardian's approval —
    // whichever the database actually serializes first is authoritative;
    // what must never happen is the row ending up with `cancelled_at` set
    // while `status` says anything other than `cancelled`.
    let cancel = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/cancel")),
        &owner_token,
    )
    .send();
    let approve = auth(
        http.post(format!("{base}/recovery/requests/{request_id}/approve")),
        &g2_token,
    )
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

    seed_friendship(&pool, owner_id, guardian_id).await;
    configure_guardians(&http, &base, &owner_token, &[guardian_id], 1).await;

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
    configure_guardians(&http, &base, &owner_token, &[g1_id, g2_id], 2).await;

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
