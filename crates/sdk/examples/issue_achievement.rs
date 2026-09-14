//! Issuing an achievement as an integrator (issue #49) — the concrete
//! shape `docs/developers/achievements.md` walks through in prose. Signs
//! the attestation locally with your own issuer key; the server never sees
//! the private key, only a detached signature.
//!
//! Requires an integrator already registered (`avalon register-integrator`
//! or `POST /integrations`), an achievement already defined against it
//! (`POST /integrations/{slug}/achievements`), and the target identity
//! having granted `achievements.issue` (`POST /integrations/{slug}/connect`).
//! `avalon-cli`'s own `issue-achievement` command drives this exact SDK
//! call — see `docs/developers/local-development.md` for a local walkthrough
//! that doesn't require writing any code.
//!
//! ```text
//! AVALON_SERVER_URL=http://127.0.0.1:8080 \
//! AVALON_SESSION_TOKEN=<the target identity's session token> \
//! AVALON_INTEGRATOR_SLUG=<your integrator's slug> \
//! AVALON_INTEGRATOR_KEY_ID=<your integrator's credential key id> \
//! AVALON_INTEGRATOR_SIGNING_KEY=<your integrator's base64-encoded 32-byte Ed25519 key> \
//! AVALON_ACHIEVEMENT_KEY=<the achievement's key> \
//! cargo run -p avalon-sdk --example issue_achievement
//! ```

use avalon_sdk::{AvalonClient, AvalonConfig};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

#[tokio::main]
async fn main() {
    let server_url =
        std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let token = std::env::var("AVALON_SESSION_TOKEN").expect("set AVALON_SESSION_TOKEN");
    let slug = std::env::var("AVALON_INTEGRATOR_SLUG").expect("set AVALON_INTEGRATOR_SLUG");
    let key_id = std::env::var("AVALON_INTEGRATOR_KEY_ID").expect("set AVALON_INTEGRATOR_KEY_ID");
    let achievement_key =
        std::env::var("AVALON_ACHIEVEMENT_KEY").expect("set AVALON_ACHIEVEMENT_KEY");

    // Never a bare CLI argument — read from the environment (or, in a real
    // integration, from a secrets manager/file), same invariant
    // `avalon-cli`'s own `issue-achievement` command follows.
    let signing_key_base64 =
        std::env::var("AVALON_INTEGRATOR_SIGNING_KEY").expect("set AVALON_INTEGRATOR_SIGNING_KEY");
    let signing_key: [u8; 32] = BASE64
        .decode(signing_key_base64.trim())
        .expect("AVALON_INTEGRATOR_SIGNING_KEY should be valid base64")
        .try_into()
        .expect("AVALON_INTEGRATOR_SIGNING_KEY should decode to exactly 32 bytes");

    let client = AvalonClient::new(AvalonConfig {
        server_url,
        integrator_credential_key_id: key_id,
        integrator_slug: Some(slug),
        signing_key: Some(signing_key),
        retry: Default::default(),
    });

    let session = client
        .authenticate(&token)
        .await
        .expect("authenticate() failed — is AVALON_SESSION_TOKEN valid and unexpired?");

    let attestation_id = session.issue_achievement(&achievement_key).await.expect(
        "issue_achievement() failed — see this example's own doc comment for prerequisites",
    );

    println!("Issued '{achievement_key}' to {}.", session.identity().id.0);
    println!("Attestation id: {attestation_id}");
}
