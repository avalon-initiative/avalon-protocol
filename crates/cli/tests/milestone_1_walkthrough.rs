//! Automated equivalent of `docs/maintainers/milestone-1-walkthrough.md`
//! (issue #64) — one continuous run of `Proposal.md` §23's fourteen-step
//! milestone-1 vertical slice, plus #64's two extra checks (ledger
//! integrity, rebuild-from-ledger), against a real, running `avalon-server`
//! and Postgres. Gated `--ignored` since it needs live infra — see
//! `make test-live` / `make start`. Never runs under `make test`.
//!
//! Lives in `avalon-cli`'s own `tests/` (not `avalon-sdk`'s) so
//! `env!("CARGO_BIN_EXE_avalon")` resolves to the real built binary —
//! that only works for a binary's own package, and `register-game`/
//! `inspect-ledger` (steps 8/11/15) are meant to run as real subprocesses,
//! exactly as an operator would run them from a shell.
//!
//! Three layers, matching who actually does each step:
//! - [`hub_side`]: raw HTTP only, mirroring what the Hub does (identity
//!   creation, friends, guilds, channels, reads) — no `avalon_sdk`.
//! - [`game_side`]: `avalon_sdk` + `avalon_protocol` only — enforced by
//!   this module simply never importing `avalon_server`, `avalon_chain`,
//!   or `sqlx` — mirroring what a game does (authenticate, issue, read,
//!   verify, decide what to recognize).
//! - the top-level test itself: spawns the real `avalon` binary for the
//!   two CLI-owned operator steps (`register-integrator`/`register-game`,
//!   `inspect-ledger`), and opens Postgres directly for step 16's
//!   rebuild-from-ledger. The separate `tampering_a_ledger_entry_breaks_the_chain`
//!   test below also opens Postgres directly, but only against its own
//!   throwaway schema — see its own doc comment for why.

mod hub_side {
    //! Everything the Hub would do, over raw HTTP — no `avalon_sdk`.

    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
    use passkey_client::{Client, DefaultClientData, Origin};
    use passkey_types::ctap2::Aaguid;
    use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
    use serde_json::json;
    use uuid::Uuid;

    pub fn server_url() -> String {
        std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
    }

    fn webauthn_origin() -> String {
        std::env::var("AVALON_WEBAUTHN_ORIGIN")
            .unwrap_or_else(|_| "http://localhost:8080".to_string())
    }

    /// Must match `avalon-server`'s `handlers::identity_created_signing_bytes`
    /// exactly — duplicated here since this module doesn't depend on
    /// `avalon-server`, same convention as `crates/sdk/tests/authenticate.rs`.
    fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
    }

    pub struct Player {
        pub identity_id: Uuid,
        pub token: String,
        /// #697/#698: the same key `register_finish` registered as this
        /// identity's first `identity_signing_keys` row — lets a later
        /// signature-required call (e.g. `POST /integrations/{slug}/connect`)
        /// sign for real.
        pub signing_key: SigningKey,
        pub signing_key_id: String,
    }

    /// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
    pub fn sign_connect(signing_key: &SigningKey, slug: &str, capabilities: &[&str]) -> String {
        let message = format!(
            "avalon:integration.connect:v1:{slug}:{}",
            capabilities.join(",")
        );
        BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
    }

    /// Steps 1/2: a real WebAuthn registration ceremony (software
    /// authenticator standing in for a browser+passkey, per
    /// `authenticate.rs`'s own comment) followed by a real login, over
    /// HTTP only — exactly what the Hub does before any game ever sees
    /// this player.
    pub async fn create_identity_and_login(
        http: &reqwest::Client,
        base: &str,
        display_name: &str,
    ) -> Player {
        let identity_id = Uuid::new_v4();
        let origin_url = url::Url::parse(&webauthn_origin()).expect("bad webauthn origin");

        let mut csprng = rand::rng();
        let signing_key = SigningKey::generate(&mut csprng);
        let event_signing_public_key = BASE64.encode(signing_key.verifying_key().to_bytes());

        let store = MemoryStore::new();
        let user_mock = MockUserValidationMethod::verified_user(2);
        let authenticator = Authenticator::new(Aaguid::new_empty(), store, user_mock);
        let mut client = Client::new(authenticator).allows_insecure_localhost(true);

        let start: serde_json::Value = http
            .post(format!("{base}/identities/register/start"))
            .json(&json!({ "identity_id": identity_id, "display_name": display_name }))
            .send()
            .await
            .expect("register/start failed — is `make start` running?")
            .json()
            .await
            .expect("register/start response was not JSON");
        let ticket_id = start["ticket_id"].as_str().unwrap().to_string();
        let creation_options: CredentialCreationOptions =
            serde_json::from_value(start["challenge"].clone()).expect("bad creation challenge");

        let webauthn_credential = client
            .register(
                Origin::from(&origin_url),
                creation_options,
                DefaultClientData,
            )
            .await
            .expect("step 1/2: WebAuthn registration ceremony failed");

        let signing_bytes = identity_created_signing_bytes(identity_id, display_name);
        let signature = signing_key.sign(&signing_bytes);

        let register_finish = http
            .post(format!("{base}/identities/register/finish"))
            .json(&json!({
                "ticket_id": ticket_id,
                "webauthn_credential": webauthn_credential,
                "event_signing_public_key": event_signing_public_key,
                "event_signature": BASE64.encode(signature.to_bytes()),
            }))
            .send()
            .await
            .expect("register/finish request failed");
        assert!(
            register_finish.status().is_success(),
            "step 1/2: register/finish failed: {:?}",
            register_finish.status()
        );

        let session_start: serde_json::Value = http
            .post(format!("{base}/sessions/start"))
            .json(&json!({ "identity_id": identity_id }))
            .send()
            .await
            .expect("sessions/start request failed")
            .json()
            .await
            .expect("sessions/start response was not JSON");
        let session_ticket_id = session_start["ticket_id"].as_str().unwrap().to_string();
        let request_options: CredentialRequestOptions =
            serde_json::from_value(session_start["challenge"].clone())
                .expect("bad request challenge");

        let assertion = client
            .authenticate(
                Origin::from(&origin_url),
                request_options,
                DefaultClientData,
            )
            .await
            .expect("step 1/2: WebAuthn login ceremony failed");

        let session_finish = http
            .post(format!("{base}/sessions/finish"))
            .json(&json!({ "ticket_id": session_ticket_id, "credential": assertion }))
            .send()
            .await
            .expect("sessions/finish request failed");
        assert!(
            session_finish.status().is_success(),
            "step 1/2: sessions/finish failed: {:?}",
            session_finish.status()
        );
        let login_body: serde_json::Value = session_finish
            .json()
            .await
            .expect("sessions/finish response was not JSON");
        let token = login_body["token"]
            .as_str()
            .expect("sessions/finish response missing token")
            .to_string();

        // register_finish's own event_signing_public_key becomes this
        // identity's first identity_signing_keys row — find its server-
        // assigned id (GET /me/devices, match on public key) so a later
        // signature-required call can name it.
        let devices: serde_json::Value = http
            .get(format!("{base}/me/devices"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("GET /me/devices failed")
            .json()
            .await
            .expect("GET /me/devices response was not JSON");
        let signing_key_id = devices
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["public_key"].as_str() == Some(event_signing_public_key.as_str()))
            .expect("register_finish's signing key should be listed")["id"]
            .as_str()
            .unwrap()
            .to_string();

        Player {
            identity_id,
            token,
            signing_key,
            signing_key_id,
        }
    }

    /// Step 3: Alice and Bob become friends.
    pub async fn become_friends(http: &reqwest::Client, base: &str, a: &Player, b: &Player) {
        let create = http
            .post(format!("{base}/friends/requests"))
            .bearer_auth(&a.token)
            .json(&json!({ "to": b.identity_id }))
            .send()
            .await
            .expect("step 3: create friend request failed");
        assert!(
            create.status().is_success(),
            "step 3: create friend request failed: {:?}",
            create.status()
        );
        let body: serde_json::Value = create.json().await.unwrap();
        let request_id = body["id"].as_str().unwrap();

        let accept = http
            .post(format!("{base}/friends/requests/{request_id}/accept"))
            .bearer_auth(&b.token)
            .send()
            .await
            .expect("step 3: accept friend request failed");
        assert!(
            accept.status().is_success(),
            "step 3: accept friend request failed: {:?}",
            accept.status()
        );
    }

    /// Step 4: Alice creates a guild. Returns the guild id.
    pub async fn create_guild(
        http: &reqwest::Client,
        base: &str,
        owner: &Player,
        tag: &str,
    ) -> Uuid {
        let response = http
            .post(format!("{base}/guilds"))
            .bearer_auth(&owner.token)
            .json(&json!({
                "name": format!("Milestone 1 Guild {tag}"),
                "tag": tag,
                "description": "milestone-1 walkthrough test guild",
            }))
            .send()
            .await
            .expect("step 4: create guild failed");
        assert!(
            response.status().is_success(),
            "step 4: create guild failed: {:?}",
            response.status()
        );
        let body: serde_json::Value = response.json().await.unwrap();
        Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
    }

    /// Step 5: Bob joins the guild Alice created, via a real invite +
    /// accept round trip (works regardless of the guild's join policy,
    /// unlike `POST /guilds/{id}/join`, which only works for open guilds).
    pub async fn join_guild_via_invite(
        http: &reqwest::Client,
        base: &str,
        owner: &Player,
        joiner: &Player,
        guild_id: Uuid,
    ) {
        let invite = http
            .post(format!("{base}/guilds/{guild_id}/invites"))
            .bearer_auth(&owner.token)
            .json(&json!({ "to": joiner.identity_id }))
            .send()
            .await
            .expect("step 5: create invite failed");
        assert!(
            invite.status().is_success(),
            "step 5: create invite failed: {:?}",
            invite.status()
        );
        let body: serde_json::Value = invite.json().await.unwrap();
        let invite_id = body["id"].as_str().unwrap();

        let accept = http
            .post(format!(
                "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
            ))
            .bearer_auth(&joiner.token)
            .send()
            .await
            .expect("step 5: accept invite failed");
        assert!(
            accept.status().is_success(),
            "step 5: accept invite failed: {:?}",
            accept.status()
        );
    }

    /// Step 6: the guild creates a (non-default) channel. Returns the
    /// channel id.
    pub async fn create_channel(
        http: &reqwest::Client,
        base: &str,
        owner: &Player,
        guild_id: Uuid,
        name: &str,
    ) -> Uuid {
        let response = http
            .post(format!("{base}/guilds/{guild_id}/channels"))
            .bearer_auth(&owner.token)
            .json(&json!({ "name": name }))
            .send()
            .await
            .expect("step 6: create channel failed");
        assert!(
            response.status().is_success(),
            "step 6: create channel failed: {:?}",
            response.status()
        );
        let body: serde_json::Value = response.json().await.unwrap();
        Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
    }

    /// Step 7: Alice and Bob communicate in the channel Alice's guild
    /// created. Returns the messages read back, in the same call, as the
    /// Hub's own channel view would.
    pub async fn communicate(
        http: &reqwest::Client,
        base: &str,
        guild_id: Uuid,
        channel_id: Uuid,
        a: &Player,
        b: &Player,
    ) -> Vec<serde_json::Value> {
        let send_a = http
            .post(format!(
                "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
            ))
            .bearer_auth(&a.token)
            .json(&json!({ "body": "milestone 1: hello from alice" }))
            .send()
            .await
            .expect("step 7: alice's send failed");
        assert!(
            send_a.status().is_success(),
            "step 7: alice's send failed: {:?}",
            send_a.status()
        );

        let send_b = http
            .post(format!(
                "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
            ))
            .bearer_auth(&b.token)
            .json(&json!({ "body": "milestone 1: hello back from bob" }))
            .send()
            .await
            .expect("step 7: bob's send failed");
        assert!(
            send_b.status().is_success(),
            "step 7: bob's send failed: {:?}",
            send_b.status()
        );

        let messages = http
            .get(format!(
                "{base}/guilds/{guild_id}/channels/{channel_id}/messages"
            ))
            .bearer_auth(&a.token)
            .send()
            .await
            .expect("step 7: reading messages back failed")
            .json::<Vec<serde_json::Value>>()
            .await
            .expect("step 7: messages response was not JSON");
        messages
    }

    /// Step 9's consent half, and step 12's for Game B: the player
    /// connects to an integrator and grants it exactly `capabilities` —
    /// the real binding/consent flow (#83/#27), driven the way the Hub
    /// would drive it (bearer token, no integrator credentials involved).
    pub async fn consent_to_integrator(
        http: &reqwest::Client,
        base: &str,
        player: &Player,
        integrator_slug: &str,
        capabilities: &[&str],
    ) {
        let response = http
            .post(format!("{base}/integrations/{integrator_slug}/connect"))
            .bearer_auth(&player.token)
            .json(&json!({
                "capabilities": capabilities,
                "signing_key_id": player.signing_key_id,
                "signature": sign_connect(&player.signing_key, integrator_slug, capabilities),
            }))
            .send()
            .await
            .expect("step 9: connect request failed");
        assert!(
            response.status().is_success(),
            "step 9: connect (consent) failed: {:?}",
            response.status()
        );
    }

    /// Step 14: everything the Hub's own views would read to display
    /// identity, friends, guild, and achievements — read over the same
    /// public HTTP surface the Hub uses, nothing internal.
    pub struct HubView {
        pub display_name: String,
        pub friend_ids: Vec<Uuid>,
        pub guild_ids: Vec<Uuid>,
        pub achievement_keys: Vec<String>,
    }

    pub async fn read_hub_view(http: &reqwest::Client, base: &str, player: &Player) -> HubView {
        let me: serde_json::Value = http
            .get(format!("{base}/me"))
            .bearer_auth(&player.token)
            .send()
            .await
            .expect("step 14: GET /me failed")
            .json()
            .await
            .expect("step 14: GET /me response was not JSON");
        let display_name = me["display_name"]
            .as_str()
            .expect("step 14: GET /me missing display_name")
            .to_string();

        // `GET /friends` returns each friendship's two raw sides (`a`/`b`);
        // the caller (mirroring what the SDK's own `friends()` does — see
        // `crates/sdk/src/social.rs`) picks whichever side isn't itself.
        let friends: Vec<serde_json::Value> = http
            .get(format!("{base}/friends"))
            .bearer_auth(&player.token)
            .send()
            .await
            .expect("step 14: GET /friends failed")
            .json()
            .await
            .expect("step 14: GET /friends response was not JSON");
        let friend_ids = friends
            .iter()
            .map(|f| {
                let a = Uuid::parse_str(f["a"].as_str().unwrap()).unwrap();
                let b = Uuid::parse_str(f["b"].as_str().unwrap()).unwrap();
                if a == player.identity_id {
                    b
                } else {
                    a
                }
            })
            .collect();

        let guilds: Vec<serde_json::Value> = http
            .get(format!("{base}/me/guilds"))
            .bearer_auth(&player.token)
            .send()
            .await
            .expect("step 14: GET /me/guilds failed")
            .json()
            .await
            .expect("step 14: GET /me/guilds response was not JSON");
        let guild_ids = guilds
            .iter()
            .map(|g| Uuid::parse_str(g["guild_id"].as_str().unwrap()).unwrap())
            .collect();

        let achievements: serde_json::Value = http
            .get(format!("{base}/me/achievements"))
            .bearer_auth(&player.token)
            .send()
            .await
            .expect("step 14: GET /me/achievements failed")
            .json()
            .await
            .expect("step 14: GET /me/achievements response was not JSON");
        let achievement_keys = achievements["achievements"]
            .as_array()
            .expect("step 14: GET /me/achievements missing achievements array")
            .iter()
            .map(|a| a["achievement"].as_str().unwrap().to_string())
            .collect();

        HubView {
            display_name,
            friend_ids,
            guild_ids,
            achievement_keys,
        }
    }
}

mod game_side {
    //! Everything a game does at runtime, through `avalon_sdk` +
    //! `avalon_protocol` only — no `avalon_server`, `avalon_chain`, or
    //! `sqlx` import anywhere in this module, enforced simply by the
    //! absence of a `use` for any of them.

    use avalon_protocol::achievements::{recognize, Issuer, Recognition, TrustRelationship};
    use avalon_protocol::ids::IntegratorId;
    use avalon_sdk::achievements::{Authenticity, Validity, VerifiedAttestation};
    use avalon_sdk::{AvalonClient, AvalonConfig, Session};

    /// Credentials a game holds after registering with Avalon — produced
    /// by parsing `avalon register-game`'s real stdout in this file's own
    /// top-level test, not by this module.
    pub struct GameCredentials {
        pub slug: String,
        pub integrator_id: uuid::Uuid,
        pub key_id: String,
        pub signing_key: [u8; 32],
    }

    fn client_for(server_url: &str, game: &GameCredentials) -> AvalonClient {
        AvalonClient::new(AvalonConfig {
            server_url: server_url.to_string(),
            integrator_credential_key_id: game.key_id.clone(),
            integrator_slug: Some(game.slug.clone()),
            signing_key: Some(game.signing_key),
            retry: Default::default(),
        })
    }

    /// Step 9: a game authenticates a player through the SDK, given the
    /// player's Avalon session token (handed to the game by the player,
    /// exactly as a real integration flow works — the game never sees a
    /// passkey or the player's raw identity credentials).
    pub async fn authenticate_player(
        server_url: &str,
        game: &GameCredentials,
        player_token: &str,
    ) -> Session {
        client_for(server_url, game)
            .authenticate(player_token)
            .await
            .expect("step 9: authenticate() should succeed with a valid session token")
    }

    /// Step 10: Game A issues `dragon_slayer` to the authenticated player,
    /// signed with Game A's own key, entirely through the SDK.
    pub async fn issue_dragon_slayer(session: &Session) -> uuid::Uuid {
        session
            .issue_achievement("dragon_slayer")
            .await
            .expect("step 10: issue_achievement should succeed once granted")
    }

    /// Step 12: Game B reads the player's attestation history through the
    /// SDK and reports authenticity and validity as the two separate
    /// questions they are (#76) — never merged into one boolean.
    pub async fn verify_dragon_slayer(session: &Session) -> VerifiedAttestation {
        let history = session
            .achievements()
            .await
            .expect("step 12: achievements() should succeed once granted");
        history
            .into_iter()
            .find(|a| a.achievement.ends_with(":achievement:dragon_slayer"))
            .expect("step 12: dragon_slayer should be in the player's attestation history")
    }

    pub fn is_authentic(attestation: &VerifiedAttestation) -> bool {
        matches!(attestation.authenticity, Authenticity::Authentic { .. })
    }

    pub fn is_valid(attestation: &VerifiedAttestation) -> bool {
        matches!(attestation.validity, Validity::Valid)
    }

    /// Step 13: Game B's own policy choice, entirely client-side — a
    /// server verdict never decides this (#76's third question).
    pub fn game_b_recognizes(
        game_b_id: uuid::Uuid,
        game_a_id: uuid::Uuid,
        attestation: &VerifiedAttestation,
    ) -> Recognition {
        let game_a_issuer = Issuer::Game(IntegratorId(game_a_id));
        let policy = TrustRelationship {
            truster: IntegratorId(game_b_id),
            trusted_issuer: game_a_issuer.clone(),
            established_at: attestation.issued_at,
            scopes: Vec::new(), // unscoped: trust every claim from game A
        };
        recognize(
            &policy,
            &game_a_issuer,
            "achievement",
            None,
            1,
            attestation.issued_at,
        )
    }
}

use std::path::PathBuf;
use std::process::Command;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use sqlx::postgres::PgPoolOptions;
use sqlx::AssertSqlSafe;
use uuid::Uuid;

fn cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_avalon"))
}

/// Step 8/11: `avalon register-game` (the CLI's own name for
/// `register-integrator`, #290), run as a real subprocess exactly as an
/// operator would from a shell, then its stdout parsed for the credentials
/// it prints — the server never returns the private key a second time.
fn register_game_via_cli(base: &str, slug: &str, name: &str) -> game_side::GameCredentials {
    let output = Command::new(cli_bin())
        .args([
            "register-game",
            "--slug",
            slug,
            "--name",
            name,
            "--owner-name",
            "Milestone 1 Test Studio",
            "--capability",
            "achievements.issue",
            "--capability",
            "achievements.read",
            "--server",
            base,
        ])
        .env("AVALON_SERVER_URL", base)
        .output()
        .expect("step 8/11: failed to spawn `avalon register-game`");
    assert!(
        output.status.success(),
        "step 8/11: `avalon register-game` failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let integrator_id = stdout
        .lines()
        .find_map(|line| {
            let prefix = format!("Integrator registered: {slug} (");
            line.strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(')'))
        })
        .unwrap_or_else(|| panic!("step 8/11: could not find integrator id in output:\n{stdout}"))
        .parse::<Uuid>()
        .expect("step 8/11: integrator id was not a valid uuid");

    let key_id = stdout
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("Key ID:"))
        .map(str::trim)
        .unwrap_or_else(|| panic!("step 8/11: could not find Key ID in output:\n{stdout}"))
        .to_string();

    let signing_key_base64 = stdout
        .lines()
        .skip_while(|line| !line.contains("Private signing key (base64):"))
        .nth(1)
        .map(str::trim)
        .unwrap_or_else(|| {
            panic!("step 8/11: could not find private signing key in output:\n{stdout}")
        });
    let signing_key_bytes: [u8; 32] = BASE64
        .decode(signing_key_base64)
        .expect("step 8/11: private signing key was not valid base64")
        .try_into()
        .expect("step 8/11: private signing key was not 32 bytes");

    game_side::GameCredentials {
        slug: slug.to_string(),
        integrator_id,
        key_id,
        signing_key: signing_key_bytes,
    }
}

/// Defines `dragon_slayer` for Game A. Not part of `game_side` on purpose:
/// this is one-time integrator setup done directly against the API with
/// the key `register_game_via_cli` just produced, not something a
/// consumer-facing game SDK exposes at runtime (mirrors
/// `crates/sdk/tests/achievements.rs`'s own `define_achievement` helper).
async fn define_dragon_slayer(
    http: &reqwest::Client,
    base: &str,
    game: &game_side::GameCredentials,
) {
    use ed25519_dalek::{Signer, SigningKey};

    let signing_key = SigningKey::from_bytes(&game.signing_key);
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", game.slug))
        .send()
        .await
        .expect("step 10: challenge request failed")
        .json()
        .await
        .expect("step 10: challenge response was not JSON");
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = signing_key.sign(&nonce);

    let response = http
        .post(format!("{base}/integrations/{}/achievements", game.slug))
        .header("x-avalon-integrator-key-id", &game.key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .json(&serde_json::json!({
            "key": "dragon_slayer",
            "name": "Dragon Slayer",
            "description": "Slew the dragon (milestone-1 walkthrough)",
        }))
        .send()
        .await
        .expect("step 10: define achievement request failed");
    assert!(
        response.status().is_success(),
        "step 10: defining dragon_slayer failed: {:?}",
        response.status()
    );
}

/// Identity/guild/achievement writes are atomic with their *outbox* row
/// (`crates/server/src/outbox.rs`), not with the ledger entry itself — a
/// background worker drains that outbox into `ledger_entries`
/// asynchronously. Found live while writing this test: calling
/// `avalon inspect-ledger` immediately after step 14's HTTP reads succeed
/// is a real race, since a 200 response only proves the outbox row landed,
/// not that the worker has published it yet. Polls `avalon outbox-status`
/// (parsing its own `outbox: empty, nothing pending` vs `N pending`
/// output) until it drains, so step 15 observes a ledger that has actually
/// caught up.
fn wait_for_outbox_to_drain() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let output = Command::new(cli_bin())
            .arg("outbox-status")
            .output()
            .expect("failed to spawn `avalon outbox-status`");
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("empty, nothing pending") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "outbox did not drain within 30s before step 15's ledger inspection: {stdout}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Step 15: `avalon inspect-ledger`, run as a real subprocess, its output
/// parsed for every `kind` this run should have produced and for the
/// overall chain-intact verdict.
fn inspect_ledger_output() -> String {
    let output = Command::new(cli_bin())
        .arg("inspect-ledger")
        .output()
        .expect("step 15: failed to spawn `avalon inspect-ledger`");
    assert!(
        output.status.success(),
        "step 15: `avalon inspect-ledger` failed:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Step 15's own chain-integrity check, scoped to the entries *this run*
/// produced rather than the whole shared `make start` dev ledger. Found
/// live while writing this test: this repo's own long-lived shared dev
/// database (accumulated across many sessions/agents over time, per
/// `.claude/CLAUDE.md`) already carries pre-existing broken links from
/// unrelated historical activity — a real, worth-a-ticket environment gap
/// (a long-lived shared dev ledger has no cleanup story), not a defect in
/// anything this test exercises. Asserting a blanket "chain intact" over
/// the *entire* ledger would make this test fail on unrelated history
/// forever, so instead: find every block whose `issuer`/`subject` line
/// contains one of this run's own unique markers (the run id, guild id, or
/// game slugs — all fresh `Uuid`-derived per run) and require each such
/// block to report `verified:  ✓`, never `✗ BROKEN CHAIN`. This still
/// fails on a genuine regression in how *this run's own* writes get
/// chained, which is what actually matters here.
fn assert_this_runs_entries_are_verified(ledger_output: &str, markers: &[String]) {
    let blocks: Vec<&str> = ledger_output.split("┌─ Block #").skip(1).collect();
    let mut checked = 0usize;
    for block in blocks {
        if !markers.iter().any(|m| block.contains(m.as_str())) {
            continue;
        }
        checked += 1;
        assert!(
            block.contains("verified:  \u{2713}"),
            "step 15: this run's own ledger entry is not reported as verified:\n{block}"
        );
        assert!(
            !block.contains("BROKEN CHAIN"),
            "step 15: this run's own ledger entry reports a broken chain link:\n{block}"
        );
    }
    assert!(
        checked > 0,
        "step 15: found none of this run's own entries in `avalon inspect-ledger` output at all"
    );
}

#[tokio::test]
#[ignore]
async fn milestone_1_end_to_end_vertical_slice() {
    let base = hub_side::server_url();
    let http = reqwest::Client::new();
    let run_id = Uuid::new_v4().simple().to_string();

    // Steps 1-2: Player A and Player B each create an Avalon identity.
    let alice =
        hub_side::create_identity_and_login(&http, &base, &format!("m1-alice-{run_id}")).await;
    let bob = hub_side::create_identity_and_login(&http, &base, &format!("m1-bob-{run_id}")).await;

    // Step 3: they become friends.
    hub_side::become_friends(&http, &base, &alice, &bob).await;

    // Step 4: Player A creates a guild.
    let tag = format!("M{}", &run_id[..4]).to_uppercase();
    let guild_id = hub_side::create_guild(&http, &base, &alice, &tag).await;

    // Step 5: Player B joins.
    hub_side::join_guild_via_invite(&http, &base, &alice, &bob, guild_id).await;

    // Step 6: the guild creates a channel.
    let channel_id = hub_side::create_channel(&http, &base, &alice, guild_id, "milestone-1").await;

    // Step 7: they communicate.
    let messages = hub_side::communicate(&http, &base, guild_id, channel_id, &alice, &bob).await;
    assert_eq!(messages.len(), 2, "step 7: both messages should round-trip");

    // Step 8: Game A registers with Avalon, via the real CLI binary.
    let game_a_slug = format!("m1-game-a-{run_id}");
    let game_a = register_game_via_cli(&base, &game_a_slug, "Milestone 1 Game A");
    define_dragon_slayer(&http, &base, &game_a).await;

    // Step 9: Player A authenticates through Game A — consent, then the SDK.
    hub_side::consent_to_integrator(
        &http,
        &base,
        &alice,
        &game_a.slug,
        &["achievements.issue", "achievements.read"],
    )
    .await;
    let alice_session_via_game_a =
        game_side::authenticate_player(&base, &game_a, &alice.token).await;

    // Step 10: Game A issues Dragon Slayer to Player A.
    let attestation_id = game_side::issue_dragon_slayer(&alice_session_via_game_a).await;

    // Step 11: Game B registers, also via the real CLI binary.
    let game_b_slug = format!("m1-game-b-{run_id}");
    let game_b = register_game_via_cli(&base, &game_b_slug, "Milestone 1 Game B");

    // Step 12: Player A connects to Game B too, and Game B verifies the
    // achievement Game A issued — authenticity and validity as separate
    // questions, never merged into one boolean.
    hub_side::consent_to_integrator(&http, &base, &alice, &game_b.slug, &["achievements.read"])
        .await;
    let alice_session_via_game_b =
        game_side::authenticate_player(&base, &game_b, &alice.token).await;
    let verified = game_side::verify_dragon_slayer(&alice_session_via_game_b).await;
    assert_eq!(
        verified.id, attestation_id,
        "step 12: same attestation Game A issued"
    );
    assert!(
        game_side::is_authentic(&verified),
        "step 12: dragon_slayer should be authentic — signature verifies against Game A's key"
    );
    assert!(
        game_side::is_valid(&verified),
        "step 12: dragon_slayer should be valid — not revoked, issuer active"
    );

    // Step 13: Game B chooses to recognize it — a client-side policy
    // choice, not a server verdict.
    let recognition =
        game_side::game_b_recognizes(game_b.integrator_id, game_a.integrator_id, &verified);
    assert!(
        matches!(recognition, avalon_protocol::achievements::Recognition::Recognized),
        "step 13: Game B trusts Game A unscoped, so this claim should be recognized: {recognition:?}"
    );

    // Step 14: the Hub displays identity, friends, guild, and achievement.
    let hub_view_before = hub_side::read_hub_view(&http, &base, &alice).await;
    assert_eq!(hub_view_before.display_name, format!("m1-alice-{run_id}"));
    assert!(
        hub_view_before.friend_ids.contains(&bob.identity_id),
        "step 14: the Hub should show Bob as a friend"
    );
    assert!(
        hub_view_before.guild_ids.contains(&guild_id),
        "step 14: the Hub should show the guild Player A created"
    );
    assert!(
        hub_view_before
            .achievement_keys
            .iter()
            .any(|k| k.ends_with(":achievement:dragon_slayer")),
        "step 14: the Hub should show the Dragon Slayer achievement"
    );

    // Step 15: `avalon inspect-ledger` shows every durable step as intact
    // chain entries.
    wait_for_outbox_to_drain();
    let ledger_output = inspect_ledger_output();
    for expected_kind in [
        "identity.created",
        "guild.created",
        "guild.member_added",
        "game.registered",
        "achievement.issued",
    ] {
        assert!(
            ledger_output.contains(&format!("kind:      {expected_kind}")),
            "step 15: expected a '{expected_kind}' entry in the ledger, got:\n{ledger_output}"
        );
    }
    let run_markers = vec![
        run_id.clone(),
        guild_id.to_string(),
        game_a.slug.clone(),
        game_b.slug.clone(),
    ];
    assert_this_runs_entries_are_verified(&ledger_output, &run_markers);

    // Step 16: drop every projection table and rebuild it from the ledger
    // alone, then repeat step 14's reads. The only place in this file that
    // opens Postgres directly.
    {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let pool = PgPoolOptions::new()
            .connect(&database_url)
            .await
            .expect("step 16: failed to connect to Postgres");
        let network_id = avalon_chain::PostgresSettlementProvider::read_genesis_network_id(&pool)
            .await
            .expect("step 16: failed to read genesis")
            .expect("step 16: no genesis network_id set — has `make start` ever run against this database?");
        let chain = avalon_chain::PostgresSettlementProvider::new(pool.clone(), network_id);

        let report = avalon_server::rebuild::rebuild_index_from_ledger(&chain, &pool)
            .await
            .expect("step 16: rebuild_index_from_ledger failed");
        assert!(
            report.events_applied > 0,
            "step 16: rebuild should have replayed at least this run's own events"
        );
    }

    let hub_view_after = hub_side::read_hub_view(&http, &base, &alice).await;
    assert_eq!(
        hub_view_after.display_name, hub_view_before.display_name,
        "step 16: profile should survive a rebuild unchanged"
    );
    assert!(
        hub_view_after.friend_ids.contains(&bob.identity_id),
        "step 16: friendship should survive a rebuild"
    );
    assert!(
        hub_view_after.guild_ids.contains(&guild_id),
        "step 16: guild membership should survive a rebuild"
    );
    assert!(
        hub_view_after
            .achievement_keys
            .iter()
            .any(|k| k.ends_with(":achievement:dragon_slayer")),
        "step 16: the achievement should survive a rebuild"
    );
}

/// Acceptance criterion: tampering with a ledger row must make step 15's
/// chain-intact check fail. Run against a throwaway, freshly-genesis'd
/// **schema** inside the same database (dropped at the end of this test) —
/// never the shared dev database's own `public` schema that `make start`
/// and every other live test use, since a broken hash chain there could
/// trip a real peer's mirror-watcher equivocation detection
/// (`avalon-peer`/`avalon-peer-two` both poll it) and, worse, permanently
/// poison every *later* entry's `prev_hash` chain for every other session
/// sharing this database — found live while writing this test: the
/// original design used a whole throwaway *database* (`CREATE DATABASE`),
/// which the shared sandbox's `avalon` Postgres role doesn't have
/// privilege for (`permission denied to create database`, expected — this
/// role is deliberately scoped, not a superuser). A schema needs only
/// `CREATE` on the owning database, which the role already has, and
/// migrations/table references throughout this codebase are all
/// schema-unqualified, so pointing a connection's `search_path` at a fresh
/// schema isolates it exactly as well as a separate database would —
/// same pattern already established by `crates/chain/tests/migration.rs`'s
/// `isolated_network_pool`.
#[tokio::test]
#[ignore]
async fn tampering_a_ledger_entry_breaks_the_chain() {
    use sqlx::Row;

    let base_database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let admin_pool = PgPoolOptions::new()
        .connect(&base_database_url)
        .await
        .expect("failed to connect to Postgres");

    let schema = format!("avalon_m1_tamper_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE"
    )))
    .execute(&admin_pool)
    .await
    .expect("failed to drop any stale test schema");
    sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin_pool)
        .await
        .expect("failed to create test schema");

    // Best-effort cleanup even if an assertion below panics.
    struct DropSchemaOnDrop {
        database_url: String,
        schema: String,
    }
    impl Drop for DropSchemaOnDrop {
        fn drop(&mut self) {
            let database_url = self.database_url.clone();
            let schema = self.schema.clone();
            // `block_in_place`/a dedicated runtime isn't available from a
            // plain `Drop` in an async test; best-effort via a blocking
            // std thread with its own tiny runtime.
            let _ = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("failed to build cleanup runtime");
                rt.block_on(async move {
                    if let Ok(pool) = PgPoolOptions::new().connect(&database_url).await {
                        let _ = sqlx::query(AssertSqlSafe(format!(
                            "DROP SCHEMA IF EXISTS {schema} CASCADE"
                        )))
                        .execute(&pool)
                        .await;
                    }
                });
            })
            .join();
        }
    }
    let _cleanup = DropSchemaOnDrop {
        database_url: base_database_url.clone(),
        schema: schema.clone(),
    };

    // A pool whose every connection's `search_path` resolves unqualified
    // table names into the fresh schema first, `public` second — used from
    // *inside* this test process for setup and the tamper itself.
    let owned_schema = schema.clone();
    let scratch_pool = PgPoolOptions::new()
        .after_connect(move |conn, _meta| {
            let schema = owned_schema.clone();
            Box::pin(async move {
                sqlx::query(AssertSqlSafe(format!("SET search_path = {schema}, public")))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&base_database_url)
        .await
        .expect("failed to connect scratch pool");

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let migrations_dir = manifest_dir.join("../server/db/migrations");
    avalon_server::migrate::migrate_up(&scratch_pool, &migrations_dir)
        .await
        .expect("failed to migrate scratch schema");

    let network_id = format!("avalon-m1-tamper-{schema}");
    let chain =
        avalon_chain::PostgresSettlementProvider::connect(scratch_pool.clone(), &network_id)
            .await
            .expect("failed to write genesis on scratch schema");

    let subject =
        avalon_protocol::ids::GlobalId::new("identity", "self", "identity", "tamper-test");
    let issuer = subject.clone();
    for i in 0..3 {
        let batch = avalon_protocol::events::EventBatch {
            id: Uuid::new_v4(),
            events: vec![avalon_protocol::events::ProtocolEvent {
                id: Uuid::new_v4(),
                kind: "identity.created".to_string(),
                issuer: issuer.clone(),
                subject: subject.clone(),
                payload: serde_json::json!({ "seq": i }),
                timestamp: time::OffsetDateTime::now_utc(),
                version: 1,
            }],
            created_at: time::OffsetDateTime::now_utc(),
        };
        <avalon_chain::PostgresSettlementProvider as avalon_chain::SettlementProvider>::commit(
            &chain, &batch,
        )
        .await
        .expect("failed to commit a scratch ledger entry");
    }

    // Corrupt one row's stored hash directly — an operator-invisible
    // tamper, exactly the thing `chain_intact` exists to catch.
    let target_seq: i64 = sqlx::query("SELECT seq FROM ledger_entries ORDER BY seq LIMIT 1")
        .fetch_one(&scratch_pool)
        .await
        .expect("failed to read a ledger entry to tamper with")
        .try_get("seq")
        .unwrap();
    sqlx::query("UPDATE ledger_entries SET entry_hash = 'deadbeef-tampered' WHERE seq = $1")
        .bind(target_seq)
        .execute(&scratch_pool)
        .await
        .expect("failed to tamper with ledger entry");

    // The exact same read `avalon inspect-ledger` itself performs
    // (`PostgresSettlementProvider::list_entries`, re-verifying every
    // entry's content hash and chain link) — called in-process against the
    // scratch pool rather than shelling out to the CLI with a
    // search-path-steered `DATABASE_URL`. Found live while writing this
    // test: routing the child `avalon` process's `search_path` through the
    // standard libpq `options` connection parameter is real, but this
    // sandbox's networking made that child connection hang indefinitely
    // rather than fail fast, which would make this test flaky/slow for no
    // real benefit — `list_entries`/`chain_intact` **is** what
    // `inspect-ledger`'s own "✗ BROKEN CHAIN" line is printed from
    // (`crates/cli/src/main.rs::inspect_ledger`), so this is the same
    // check, just without a second process and a second connection to
    // reason about.
    let entries = chain
        .list_entries()
        .await
        .expect("failed to list scratch ledger entries");
    assert_eq!(
        entries.len(),
        3,
        "expected exactly the 3 scratch entries committed above"
    );
    let broken_seqs: Vec<i64> = entries
        .iter()
        .filter(|e| !e.chain_intact)
        .map(|e| e.seq)
        .collect();
    assert!(
        !broken_seqs.is_empty(),
        "step 15's own check must fail against a tampered ledger row — every entry reported chain_intact=true, expected seq {target_seq} to report false"
    );
}
