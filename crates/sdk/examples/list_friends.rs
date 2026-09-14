//! Reading a player's own friends list, with presence embedded when
//! granted (issue #49) — `docs/developers/guilds-and-friends.md`'s
//! `friends()` example. Requires `friends.read`; presence is included
//! automatically only if `presence.read` is also granted (see
//! `Session::friends`'s own doc comment) — nothing extra to ask for.
//!
//! ```text
//! AVALON_SERVER_URL=http://127.0.0.1:8080 \
//! AVALON_SESSION_TOKEN=<a session token that has granted friends.read to this integrator> \
//! AVALON_INTEGRATOR_KEY_ID=<your integrator's credential key id> \
//! cargo run -p avalon-sdk --example list_friends
//! ```

use avalon_sdk::{AvalonClient, AvalonConfig, SdkError};

#[tokio::main]
async fn main() {
    let server_url =
        std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let token = std::env::var("AVALON_SESSION_TOKEN").expect("set AVALON_SESSION_TOKEN");
    let key_id = std::env::var("AVALON_INTEGRATOR_KEY_ID").unwrap_or_default();

    let client = AvalonClient::new(AvalonConfig {
        server_url,
        integrator_credential_key_id: key_id,
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    });

    let session = client
        .authenticate(&token)
        .await
        .expect("authenticate() failed — is AVALON_SESSION_TOKEN valid and unexpired?");

    match session.friends().await {
        Ok(friends) if friends.is_empty() => println!("No friends yet."),
        Ok(friends) => {
            for friend in friends {
                let presence = friend
                    .presence
                    .map(|p| format!("{:?}", p.status))
                    .unwrap_or_else(|| "unknown (presence.read not granted)".to_string());
                println!("{}: {presence}", friend.identity_id.0);
            }
        }
        // A typed error, never a raw status code — see
        // docs/developers/errors-and-retries.md.
        Err(SdkError::CapabilityNotGranted(capability)) => {
            eprintln!("this integrator hasn't been granted '{capability}' by this identity.");
            std::process::exit(1);
        }
        Err(other) => {
            eprintln!("friends() failed: {other}");
            std::process::exit(1);
        }
    }
}
