//! Exercises the social-recovery request-initiation surface (issue #201,
//! closed out by #741/#747) against a real, running `avalon-server` and
//! Postgres. Gated `--ignored` since it needs live infra — see `make
//! test-live` / `make start`.
//!
//! Setup uses the SDK's own `AvalonClient::register`/`AccountSession`
//! (issue #699) rather than hand-rolled HTTP, since both are real,
//! already-live-verified SDK surfaces — this test's whole point is
//! exercising the *new* `AvalonClient::start_recovery_request`/
//! `identity_recovery_status`/`get_recovery_request`/
//! `finalize_recovery_request` surface, not re-proving registration.

use avalon_sdk::{AvalonClient, AvalonConfig};
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn plain_client() -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: String::new(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    })
}

/// `set_guardians` requires every named guardian to already be a friend
/// (`crates/server/src/recovery.rs::set_guardians`'s own check) — this
/// drives the real friend-request/accept HTTP flow so the two accounts
/// this test registers satisfy that before naming a guardian.
async fn befriend(
    http: &reqwest::Client,
    base: &str,
    guardian_id: Uuid,
    owner_token: &str,
    guardian_token: &str,
) {
    let create = http
        .post(format!("{base}/friends/requests"))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({ "to": guardian_id }))
        .send()
        .await
        .expect("create friend request failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let request_body: serde_json::Value = create.json().await.unwrap();
    let request_id = request_body["id"].as_str().unwrap();

    let accept = http
        .post(format!("{base}/friends/requests/{request_id}/accept"))
        .bearer_auth(guardian_token)
        .send()
        .await
        .expect("accept friend request failed");
    assert!(accept.status().is_success(), "{:?}", accept.status());
}

/// `AvalonClient::start_recovery_request` (a real WebAuthn registration
/// ceremony for a brand-new device) followed by `identity_recovery_status`/
/// `get_recovery_request` (both public reads) all agreeing on the same
/// pending request — the round trip this ticket's own acceptance criteria
/// call out.
#[tokio::test]
#[ignore]
async fn start_recovery_request_then_status_reads_agree_on_the_same_pending_request() {
    let client = plain_client();
    let owner_name = format!("sdk-recovery-owner-{}", Uuid::new_v4());
    let guardian_name = format!("sdk-recovery-guardian-{}", Uuid::new_v4());

    let owner = client
        .register(&owner_name)
        .await
        .expect("register owner should succeed against a real server");
    let guardian = client
        .register(&guardian_name)
        .await
        .expect("register guardian should succeed against a real server");
    let owner_id = owner.identity().id.0;
    let guardian_id = guardian.identity().id.0;
    let http = reqwest::Client::new();
    befriend(
        &http,
        &server_url(),
        guardian_id,
        owner.token(),
        guardian.token(),
    )
    .await;

    owner
        .set_guardians(&[guardian_id], 1)
        .await
        .expect("set_guardians should succeed for the owner's own identity");

    let request = client
        .start_recovery_request(owner_id, Some("recovered laptop"))
        .await
        .expect("start_recovery_request should drive a real WebAuthn ceremony end to end");
    assert_eq!(request.identity_id, owner_id);
    assert_eq!(request.threshold, 1);
    assert_eq!(request.status, "pending_approvals");

    let by_identity = client
        .identity_recovery_status(owner_id)
        .await
        .expect("identity_recovery_status should succeed")
        .expect("a freshly started recovery request should be the identity's active one");
    assert_eq!(by_identity.id, request.id);

    let by_id = client
        .get_recovery_request(request.id)
        .await
        .expect("get_recovery_request should succeed for a real request id");
    assert_eq!(by_id.id, request.id);
    assert_eq!(by_id.status, request.status);
}

/// A guardian's real `AccountSession::approve_recovery_request` (already
/// live-verified elsewhere) advances a request past `pending_approvals`
/// into `delay` once its threshold is met, and
/// `AvalonClient::finalize_recovery_request` — called before that delay
/// has elapsed — is rejected rather than silently granting recovery early,
/// proving the mandatory public delay this ticket's design depends on is
/// still enforced through the new SDK entry point, not just the endpoint
/// it wraps.
#[tokio::test]
#[ignore]
async fn finalize_before_the_delay_elapses_is_rejected_not_silently_granted() {
    let client = plain_client();
    let owner_name = format!("sdk-recovery-early-{}", Uuid::new_v4());
    let guardian_name = format!("sdk-recovery-early-guardian-{}", Uuid::new_v4());

    let owner = client.register(&owner_name).await.unwrap();
    let guardian = client.register(&guardian_name).await.unwrap();
    let owner_id = owner.identity().id.0;
    let guardian_id = guardian.identity().id.0;
    let http = reqwest::Client::new();
    befriend(
        &http,
        &server_url(),
        guardian_id,
        owner.token(),
        guardian.token(),
    )
    .await;

    owner.set_guardians(&[guardian_id], 1).await.unwrap();

    let request = client
        .start_recovery_request(owner_id, None)
        .await
        .expect("start_recovery_request should succeed");

    let approved = guardian
        .approve_recovery_request(request.id)
        .await
        .expect("the named guardian should be able to approve a real pending request");
    assert_eq!(approved.status, "delay");
    assert!(approved.delay_ends_at.is_some());

    let result = client.finalize_recovery_request(request.id).await;
    assert!(
        matches!(result, Err(avalon_sdk::SdkError::Conflict(_))),
        "finalizing before the mandatory delay elapses must be rejected, got {result:?}"
    );
}
