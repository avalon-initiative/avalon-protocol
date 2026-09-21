//! Exercises `AccountSession` end to end against a real, running
//! `avalon-server` (issue #699) — full registration -> login -> a
//! signature-required guild-admin action, proving the auto-minted
//! signature actually verifies server-side (#698's enforcement). Gated
//! `--ignored` since it needs live infra — see `make test-live` /
//! `make start`.
//!
//! Uses a real virtual-authenticator WebAuthn ceremony throughout (no
//! direct SQL seeding of `identities`/`sessions`, unlike `social.rs`/
//! `guilds.rs`'s tests) — this test is specifically about `AccountSession`'s
//! own registration/login/signing plumbing, not just its HTTP layer.

use avalon_sdk::{AvalonClient, AvalonConfig};

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn client() -> AvalonClient {
    AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: "sdk-test".to_string(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    })
}

fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// Guild tags are globally unique and capped at 5 characters
/// (`crates/server/src/guilds.rs::validate_tag`), so `unique_name`'s own
/// UUID-suffixed shape doesn't fit — this takes 5 hex characters instead.
fn unique_tag() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..5].to_string()
}

#[tokio::test]
#[ignore]
async fn register_then_a_signature_required_guild_action_verifies_server_side() {
    let client = client();
    let display_name = unique_name("account-session-register");

    let session = client
        .register(&display_name)
        .await
        .expect("register() should succeed against a real server");

    assert_eq!(session.profile().display_name, display_name);
    assert!(
        session.signing_key_id().is_some(),
        "a freshly registered AccountSession should have resolved its own signing_key_id"
    );
    assert!(session.credentials().is_some());

    let guild = session
        .create_guild(&unique_name("guild"), &unique_tag(), "a test guild")
        .await
        .expect("create_guild should succeed");
    assert_eq!(guild.owner, session.identity().id.0);

    // `guild.role.create` is signature-required (#697/#698) — this call
    // only succeeds if `AccountSession::create_role` actually attached a
    // valid signature the server verified against
    // `signature_gate::canonical_message("guild.role.create", ...)`.
    // "Officer" is one of a new guild's own default-seeded roles
    // (`crates/server/src/guilds.rs`), so this test's role needs a name
    // that won't collide with it.
    let role = session
        .create_role(
            guild.id,
            "Quartermaster",
            &["manage_members"],
            "trusted role",
        )
        .await
        .expect("create_role should succeed with an auto-minted signature");
    assert_eq!(role.name, "Quartermaster");
    assert_eq!(role.permissions, vec!["manage_members".to_string()]);
}

#[tokio::test]
#[ignore]
async fn register_then_account_login_then_a_signature_required_action_round_trips() {
    let client = client();
    let display_name = unique_name("account-session-login");

    let registered = client
        .register(&display_name)
        .await
        .expect("register() should succeed against a real server");
    let credentials = registered
        .credentials()
        .expect("register() should return usable AccountCredentials")
        .clone();

    let session = client
        .account_login(&credentials)
        .await
        .expect("account_login() should succeed with credentials register() returned");
    assert_eq!(session.identity().id, registered.identity().id);
    assert!(session.signing_key_id().is_some());

    let guild = session
        .create_guild(&unique_name("guild"), &unique_tag(), "another test guild")
        .await
        .expect("create_guild should succeed after account_login");

    // `guild.transfer_ownership` is signature-required (#697/#698) —
    // transferring to a second freshly-registered identity and back proves
    // the session logged back in via `account_login` still signs correctly.
    let other = client
        .register(&unique_name("account-session-other"))
        .await
        .expect("registering a second identity should succeed");

    let transferred = session
        .transfer_ownership(guild.id, other.identity().id.0)
        .await
        .expect("transfer_ownership should succeed with an auto-minted signature");
    assert_eq!(transferred.owner, other.identity().id.0);
}

#[tokio::test]
#[ignore]
async fn resume_account_session_signs_when_given_the_signing_key() {
    let client = client();
    let display_name = unique_name("account-session-resume");

    let registered = client
        .register(&display_name)
        .await
        .expect("register() should succeed against a real server");
    let token = registered.token().to_string();
    let credentials = registered
        .credentials()
        .expect("register() should return usable AccountCredentials")
        .clone();
    let seed_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &credentials.signing_key_seed_base64,
    )
    .expect("signing_key_seed_base64 should be valid base64");
    let seed: [u8; 32] = seed_bytes
        .try_into()
        .expect("signing_key_seed_base64 should decode to 32 bytes");

    let resumed = client
        .resume_account_session_with_signing_key(&token, seed)
        .await
        .expect("resume_account_session_with_signing_key should succeed");
    assert_eq!(resumed.identity().id, registered.identity().id);
    assert_eq!(resumed.signing_key_id(), registered.signing_key_id());

    let guild = resumed
        .create_guild(
            &unique_name("guild"),
            &unique_tag(),
            "a resumed-session test guild",
        )
        .await
        .expect("create_guild should succeed on a resumed session");
    let role = resumed
        .create_role(guild.id, "Resumed", &[], "")
        .await
        .expect("create_role should succeed on a resumed session with a signing key");
    assert_eq!(role.name, "Resumed");
}

#[tokio::test]
#[ignore]
async fn resume_account_session_without_a_signing_key_sends_unsigned_and_is_rejected() {
    let client = client();
    let display_name = unique_name("account-session-resume-unsigned");

    let registered = client
        .register(&display_name)
        .await
        .expect("register() should succeed against a real server");
    let token = registered.token().to_string();

    let resumed = client
        .resume_account_session(&token)
        .await
        .expect("resume_account_session should succeed");
    assert!(resumed.signing_key_id().is_none());

    let guild = resumed
        .create_guild(
            &unique_name("guild"),
            &unique_tag(),
            "an unsigned-resume test guild",
        )
        .await
        .expect("create_guild itself is not signature-required");

    // No local signing key at all on this session -> the server rejects
    // `guild.role.create` (signature-required) with NO_REGISTERED_SIGNING_KEY
    // is wrong here (the identity does have a registered key, just not one
    // this session holds) -> FRESH_SIGNATURE_REQUIRED.
    let result = resumed.create_role(guild.id, "ShouldFail", &[], "").await;
    assert!(
        result.is_err(),
        "creating a role with no local signing key should be rejected server-side"
    );
}
