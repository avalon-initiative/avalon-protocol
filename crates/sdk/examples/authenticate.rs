//! Ten-minute "hello world" for the Avalon Rust SDK (issue #49) — add the
//! crate, build a client, authenticate, read a profile. No capability grant
//! needed for this much: `GET /me` only requires a valid session token.
//!
//! Run against a real server (`make start`) with a real identity's session
//! token:
//!
//! ```text
//! AVALON_SERVER_URL=http://127.0.0.1:8080 \
//! AVALON_SESSION_TOKEN=<token from `avalon login <identity_id>`> \
//! cargo run -p avalon-sdk --example authenticate
//! ```
//!
//! See `docs/developers/getting-started.md` for the full walkthrough,
//! including how to get a session token in the first place.

use avalon_sdk::{AvalonClient, AvalonConfig};

#[tokio::main]
async fn main() {
    let server_url =
        std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let token = std::env::var("AVALON_SESSION_TOKEN")
        .expect("set AVALON_SESSION_TOKEN — see this example's own doc comment");

    // `integrator_credential_key_id` identifies which integrator's grants
    // `authenticate()` looks up (`GET /me/grants`) — an empty/placeholder
    // value here just means "no grants," not an error; this example never
    // calls anything capability-gated.
    let client = AvalonClient::new(AvalonConfig {
        server_url,
        integrator_credential_key_id: String::new(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    });

    let session = client
        .authenticate(&token)
        .await
        .expect("authenticate() failed — is AVALON_SESSION_TOKEN valid and unexpired?");

    let profile = session.profile();
    println!("Authenticated as: {}", profile.display_name);
    println!("Identity id:       {}", session.identity().id.0);
    if let Some(bio) = &profile.bio {
        println!("Bio:               {bio}");
    }
}
