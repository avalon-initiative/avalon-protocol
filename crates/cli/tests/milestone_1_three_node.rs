//! The 3-node variant of the milestone-1 walkthrough: the
//! same real social/achievement story, but proving it works across
//! genuinely separate, independently-deployed nodes rather than one
//! process. Gated `--ignored` since it needs this sandbox's own live
//! 3-node topology (this sandbox + `avalon-peer` + `avalon-peer-two`),
//! not just `make start`. Never runs under
//! `make test` or `make test-live`.
//!
//! `crates/cli/tests/milestone_1_walkthrough.rs` already proves the
//! full vertical slice against one node — this test doesn't repeat that
//! proof, it proves a different thing: that an identity has no
//! operationally-relevant "home node" (`docs/architecture/nodes.md`'s own
//! claim), by actually relying on it. Player A registers on Node 1 only.
//! Every other action she takes — friending Player B, forming a guild,
//! receiving an achievement — happens on Node 2, a node that has *never
//! seen her before*, reached only through a real `POST
//! /auth/cross-node/submit` whose verification Node 2 can only
//! complete by fetching her signing key cross-shard from Node 1 over the
//! real DHT locator and a signed inclusion proof — not two
//! local processes sharing one Postgres via schema isolation, genuinely
//! separate, independently-deployed infrastructure.
//!
//! Reuses the exact same three-layer shape the walkthrough established (`hub_side`:
//! raw HTTP; `game_side`: `avalon_sdk`/`avalon_protocol` only; the
//! top-level test drives the real `avalon` CLI binary) — duplicated here
//! rather than shared via a `tests/common` module, matching this crate's
//! own existing precedent (`crates/sdk/tests/authenticate.rs`'s duplicated
//! signing-bytes helper) for why a `tests/*.rs` integration test doesn't
//! reach into a sibling test binary.
//!
//! The cross-node-login grant itself is minted with raw HTTP + `ed25519-dalek`
//! directly, not `avalon_sdk::AvalonClient::submit_cross_node_login_grant`
//! — that method lives on a client built for an *integrator*
//! (`AvalonConfig::integrator_credential_key_id` is required, unused for
//! this call), and this is a player action, not an integrator one; going
//! straight to the wire types (`avalon_protocol::cross_node_login`) avoids
//! constructing an SDK client around credentials this step doesn't have or
//! need — the same reasoning `hub_side` is raw-HTTP-only in #65 already.

mod hub_side {
    //! Everything the Hub would do, over raw HTTP — no `avalon_sdk`.
    //! Identical to #65's own `hub_side` module.

    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
    use passkey_client::{Client, DefaultClientData, Origin};
    use passkey_types::ctap2::Aaguid;
    use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
    use serde_json::json;
    use uuid::Uuid;

    pub fn webauthn_origin() -> String {
        std::env::var("AVALON_WEBAUTHN_ORIGIN")
            .unwrap_or_else(|_| "http://localhost:8080".to_string())
    }

    fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
        format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
    }

    pub struct Player {
        pub identity_id: Uuid,
        pub token: String,
    }

    /// A player's own real signing key, and the id the identity's very
    /// first `identity.signing_key_added` entry gave it — fetched via
    /// `GET /me/devices` right after registration, since #670's whole
    /// point needs it (to mint a real cross-node-login grant later), which
    /// #65's own `Player` never had to carry.
    pub struct PlayerKey {
        pub signing_key_id: Uuid,
        pub signing_key: SigningKey,
    }

    pub async fn create_identity_and_login(
        http: &reqwest::Client,
        base: &str,
        display_name: &str,
    ) -> (Player, PlayerKey) {
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
            .expect("register/start failed — is the target node reachable?")
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
            .expect("WebAuthn registration ceremony failed");

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
            "register/finish failed: {:?}",
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
            .expect("WebAuthn login ceremony failed");

        let session_finish = http
            .post(format!("{base}/sessions/finish"))
            .json(&json!({ "ticket_id": session_ticket_id, "credential": assertion }))
            .send()
            .await
            .expect("sessions/finish request failed");
        assert!(
            session_finish.status().is_success(),
            "sessions/finish failed: {:?}",
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
            .and_then(|arr| arr.first())
            .and_then(|d| d["id"].as_str())
            .expect("GET /me/devices should list the registering key")
            .parse::<Uuid>()
            .expect("device id was not a valid uuid");

        (
            Player { identity_id, token },
            PlayerKey {
                signing_key_id,
                signing_key,
            },
        )
    }

    pub async fn become_friends(http: &reqwest::Client, base: &str, a: &Player, b: &Player) {
        let create = http
            .post(format!("{base}/friends/requests"))
            .bearer_auth(&a.token)
            .json(&json!({ "to": b.identity_id }))
            .send()
            .await
            .expect("friend request failed");
        assert!(
            create.status().is_success(),
            "friend request failed: {:?}",
            create.status()
        );
        let body: serde_json::Value = create.json().await.unwrap();
        let request_id = body["id"].as_str().unwrap();

        let accept = http
            .post(format!("{base}/friends/requests/{request_id}/accept"))
            .bearer_auth(&b.token)
            .send()
            .await
            .expect("friend accept failed");
        assert!(
            accept.status().is_success(),
            "friend accept failed: {:?}",
            accept.status()
        );
    }

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
                "name": format!("Milestone 1 (3-node) Guild {tag}"),
                "tag": tag,
                "description": "3-node milestone-1 walkthrough test guild",
            }))
            .send()
            .await
            .expect("create guild failed");
        assert!(
            response.status().is_success(),
            "create guild failed: {:?}",
            response.status()
        );
        let body: serde_json::Value = response.json().await.unwrap();
        Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
    }

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
            .expect("create invite failed");
        assert!(
            invite.status().is_success(),
            "create invite failed: {:?}",
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
            .expect("accept invite failed");
        assert!(
            accept.status().is_success(),
            "accept invite failed: {:?}",
            accept.status()
        );
    }

    /// Mirrors `crate::signature_gate::canonical_message` byte-for-byte.
    fn sign_connect(signing_key: &SigningKey, slug: &str, capabilities: &[&str]) -> String {
        let message = format!(
            "avalon:integration.connect:v1:{slug}:{}",
            capabilities.join(",")
        );
        BASE64.encode(signing_key.sign(message.as_bytes()).to_bytes())
    }

    pub async fn consent_to_integrator(
        http: &reqwest::Client,
        base: &str,
        player: &Player,
        player_key: &PlayerKey,
        integrator_slug: &str,
        capabilities: &[&str],
    ) {
        let response = http
            .post(format!("{base}/integrations/{integrator_slug}/connect"))
            .bearer_auth(&player.token)
            .json(&json!({
                "capabilities": capabilities,
                "signing_key_id": player_key.signing_key_id,
                "signature": sign_connect(&player_key.signing_key, integrator_slug, capabilities),
            }))
            .send()
            .await
            .expect("connect request failed");
        assert!(
            response.status().is_success(),
            "connect (consent) failed: {:?}",
            response.status()
        );
    }

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
            .expect("GET /me failed")
            .json()
            .await
            .expect("GET /me response was not JSON");
        let display_name = me["display_name"]
            .as_str()
            .expect("GET /me missing display_name")
            .to_string();

        let friends: Vec<serde_json::Value> = http
            .get(format!("{base}/friends"))
            .bearer_auth(&player.token)
            .send()
            .await
            .expect("GET /friends failed")
            .json()
            .await
            .expect("GET /friends response was not JSON");
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
            .expect("GET /me/guilds failed")
            .json()
            .await
            .expect("GET /me/guilds response was not JSON");
        let guild_ids = guilds
            .iter()
            .map(|g| Uuid::parse_str(g["guild_id"].as_str().unwrap()).unwrap())
            .collect();

        let achievements: serde_json::Value = http
            .get(format!("{base}/me/achievements"))
            .bearer_auth(&player.token)
            .send()
            .await
            .expect("GET /me/achievements failed")
            .json()
            .await
            .expect("GET /me/achievements response was not JSON");
        let achievement_keys = achievements["achievements"]
            .as_array()
            .expect("GET /me/achievements missing achievements array")
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

    /// Polls `verifier_base`'s own `GET /identities/{id}/locations`
    /// until it resolves `owner_base`'s real location — the real DHT
    /// propagation this test depends on, not a fixed sleep. Same pattern
    /// `crates/server/tests/cross_node_login_cross_shard.rs` already
    /// established for two local processes, here against real,
    /// independently-deployed nodes instead.
    pub async fn wait_for_locator_propagation(
        http: &reqwest::Client,
        verifier_base: &str,
        owner_base: &str,
        identity_id: Uuid,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            let response: serde_json::Value = http
                .get(format!(
                    "{verifier_base}/identities/{identity_id}/locations"
                ))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let locations: Vec<String> = response["locations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            if locations.iter().any(|l| l == owner_base) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "{verifier_base}'s own locator never resolved {owner_base} for identity \
                     {identity_id} within the wait budget — real DHT propagation between the \
                     live nodes never completed"
                );
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Issue #629/#656's own finding, hit live while writing this test:
    /// the outbox worker (`crate::outbox::run_worker`) drains into the real
    /// hash-chained ledger asynchronously (`AVALON_OUTBOX_POLL_INTERVAL_SECS`,
    /// 3s default) — independent of, and not necessarily in step with,
    /// #635's DHT locator propagation, which can resolve *before* the
    /// outbox worker has caught up. A cross-node login attempted in that
    /// window can genuinely 401 even though the locator already resolved
    /// correctly, because there's nothing yet for the destination node's
    /// cross-shard fetch to find. Polls `owner_base`'s own
    /// `GET /ledger/entries?subject=...&shard_id=core` (the exact query
    /// `crate::cross_shard_fetch` itself makes) until the identity's real
    /// `identity.signing_key_added` entry actually exists there.
    pub async fn wait_for_signing_key_ledger_entry(
        http: &reqwest::Client,
        owner_base: &str,
        identity_id: Uuid,
    ) {
        let subject = format!("identity:{identity_id}:self:signing_key_added");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let entries: serde_json::Value = http
                .get(format!("{owner_base}/ledger/entries"))
                .query(&[
                    ("subject", subject.as_str()),
                    ("shard_id", "core"),
                    ("limit", "10"),
                ])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if entries.as_array().is_some_and(|arr| !arr.is_empty()) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "{owner_base}'s own outbox worker never materialized the \
                     signing_key_added event into the real ledger within the wait budget"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Epic #623: mints and submits a same-device-fast-path
    /// `CrossNodeLoginGrant` directly against `destination_base`, using the
    /// identity's own real signing key — raw HTTP + `ed25519-dalek`, not
    /// `avalon_sdk` (see this file's own module doc comment for why).
    /// Returns the real session token `destination_base` minted.
    pub async fn cross_node_login(
        http: &reqwest::Client,
        destination_base: &str,
        identity_id: Uuid,
        key: &PlayerKey,
    ) -> String {
        let issued_at = time::OffsetDateTime::now_utc();
        let expires_at = issued_at
            + time::Duration::seconds(avalon_protocol::cross_node_login::DEFAULT_TTL_SECONDS);
        let nonce = Uuid::new_v4();
        let bytes = avalon_protocol::cross_node_login::signing_bytes(
            identity_id,
            key.signing_key_id,
            destination_base,
            destination_base,
            nonce,
            issued_at,
            expires_at,
        );
        let signature = key.signing_key.sign(&bytes);
        let grant = avalon_protocol::cross_node_login::CrossNodeLoginGrant {
            identity_id,
            signing_key_id: key.signing_key_id,
            destination_base_url: destination_base.to_string(),
            requesting_context: destination_base.to_string(),
            nonce,
            issued_at,
            expires_at,
            signature: hex::encode(signature.to_bytes()),
        };

        let submit = http
            .post(format!("{destination_base}/auth/cross-node/submit"))
            .json(&json!({ "grant": grant }))
            .send()
            .await
            .expect("cross-node submit request failed");
        assert!(
            submit.status().is_success(),
            "cross-node-login submit against {destination_base} should have succeeded (real \
             cross-shard verification against the identity's home node): {:?} {}",
            submit.status(),
            submit
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string())
        );
        let body: serde_json::Value = submit.json().await.unwrap();
        body["token"]
            .as_str()
            .expect("cross-node-login submit response should carry a real session token")
            .to_string()
    }
}

mod game_side {
    //! Everything a game does at runtime, through `avalon_sdk` +
    //! `avalon_protocol` only. Identical to #65's own `game_side` module.

    use avalon_protocol::achievements::{recognize, Issuer, Recognition, TrustRelationship};
    use avalon_protocol::ids::IntegratorId;
    use avalon_sdk::achievements::{Authenticity, Validity, VerifiedAttestation};
    use avalon_sdk::{AvalonClient, AvalonConfig, Session};

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

    pub async fn authenticate_player(
        server_url: &str,
        game: &GameCredentials,
        player_token: &str,
    ) -> Session {
        client_for(server_url, game)
            .authenticate(player_token)
            .await
            .expect("authenticate() should succeed with a valid session token")
    }

    pub async fn issue_dragon_slayer(session: &Session) -> uuid::Uuid {
        session
            .issue_achievement("dragon_slayer")
            .await
            .expect("issue_achievement should succeed once granted")
    }

    pub async fn verify_dragon_slayer(session: &Session) -> VerifiedAttestation {
        let history = session
            .achievements()
            .await
            .expect("achievements() should succeed once granted");
        history
            .into_iter()
            .find(|a| a.achievement.ends_with(":achievement:dragon_slayer"))
            .expect("dragon_slayer should be in the player's attestation history")
    }

    pub fn is_authentic(attestation: &VerifiedAttestation) -> bool {
        matches!(attestation.authenticity, Authenticity::Authentic { .. })
    }

    pub fn is_valid(attestation: &VerifiedAttestation) -> bool {
        matches!(attestation.validity, Validity::Valid)
    }

    pub fn game_recognizes(
        recognizer_id: uuid::Uuid,
        issuer_id: uuid::Uuid,
        attestation: &VerifiedAttestation,
    ) -> Recognition {
        let issuer = Issuer::Game(IntegratorId(issuer_id));
        let policy = TrustRelationship {
            truster: IntegratorId(recognizer_id),
            trusted_issuer: issuer.clone(),
            established_at: attestation.issued_at,
            scopes: Vec::new(),
        };
        recognize(
            &policy,
            &issuer,
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
use uuid::Uuid;

fn cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_avalon"))
}

fn register_game_via_cli(base: &str, slug: &str, name: &str) -> game_side::GameCredentials {
    let output = Command::new(cli_bin())
        .args([
            "register-game",
            "--slug",
            slug,
            "--name",
            name,
            "--owner-name",
            "Milestone 1 (3-node) Test Studio",
            "--capability",
            "achievements.issue",
            "--capability",
            "achievements.read",
            "--server",
            base,
        ])
        .env("AVALON_SERVER_URL", base)
        .output()
        .expect("failed to spawn `avalon register-game`");
    assert!(
        output.status.success(),
        "`avalon register-game` against {base} failed:\nstdout: {}\nstderr: {}",
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
        .unwrap_or_else(|| panic!("could not find integrator id in output:\n{stdout}"))
        .parse::<Uuid>()
        .expect("integrator id was not a valid uuid");

    let key_id = stdout
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("Key ID:"))
        .map(str::trim)
        .unwrap_or_else(|| panic!("could not find Key ID in output:\n{stdout}"))
        .to_string();

    let signing_key_base64 = stdout
        .lines()
        .skip_while(|line| !line.contains("Private signing key (base64):"))
        .nth(1)
        .map(str::trim)
        .unwrap_or_else(|| panic!("could not find private signing key in output:\n{stdout}"));
    let signing_key_bytes: [u8; 32] = BASE64
        .decode(signing_key_base64)
        .expect("private signing key was not valid base64")
        .try_into()
        .expect("private signing key was not 32 bytes");

    game_side::GameCredentials {
        slug: slug.to_string(),
        integrator_id,
        key_id,
        signing_key: signing_key_bytes,
    }
}

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
        .expect("challenge request failed")
        .json()
        .await
        .expect("challenge response was not JSON");
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
            "description": "Slew the dragon (3-node milestone-1 walkthrough)",
        }))
        .send()
        .await
        .expect("define achievement request failed");
    assert!(
        response.status().is_success(),
        "defining dragon_slayer failed: {:?}",
        response.status()
    );
}

/// Node URLs for the real 3-node sandbox topology — this sandbox's own
/// server plus the two disposable dev peers.
/// Overridable via env for a future differently-addressed topology.
fn node1_url() -> String {
    std::env::var("AVALON_NODE1_URL").unwrap_or_else(|_| "http://192.168.7.113:8080".to_string())
}
fn node2_url() -> String {
    std::env::var("AVALON_NODE2_URL").unwrap_or_else(|_| "http://192.168.7.174:8080".to_string())
}
fn node3_url() -> String {
    std::env::var("AVALON_NODE3_URL").unwrap_or_else(|_| "http://192.168.7.183:8080".to_string())
}

/// The real story: Player A's only *registration* is on Node 1. Every
/// subsequent action — friending Player B, forming a guild, receiving and
/// having an achievement verified — happens on Node 2 and Node 3, nodes
/// that never saw her register, reached only through a real cross-node
/// login that Node 2/3 can only complete by fetching her signing
/// key cross-shard from Node 1 over the real DHT locator.
/// Proves `docs/architecture/nodes.md`'s own claim — "an identity has no
/// home node in any operationally ongoing sense" — against genuinely
/// separate, independently-deployed infrastructure, not two local
/// processes sharing one Postgres.
#[tokio::test]
#[ignore]
async fn identity_has_no_operational_home_node_across_real_separate_infrastructure() {
    let node1 = node1_url();
    let node2 = node2_url();
    let node3 = node3_url();
    let http = reqwest::Client::new();
    let run_id = Uuid::new_v4().simple().to_string();

    // Step 1: Player A registers on Node 1 ONLY — this is the one and
    // only place her identity is ever created.
    let (alice, alice_key) =
        hub_side::create_identity_and_login(&http, &node1, &format!("m3n-alice-{run_id}")).await;

    // Step 2: Player B registers on Node 2 — a genuinely separate node
    // with its own database, never touched by step 1.
    let (bob, _bob_key) =
        hub_side::create_identity_and_login(&http, &node2, &format!("m3n-bob-{run_id}")).await;

    // Real DHT propagation: Node 2 has to actually learn (via #635's
    // locator, over the real libp2p DHT this sandbox's three nodes
    // already bootstrap into) that Alice's identity lives at Node 1's
    // base_url before it can verify anything about her.
    hub_side::wait_for_locator_propagation(&http, &node2, &node1, alice.identity_id).await;
    // And Node 1's own outbox worker has to have actually drained Alice's
    // signing_key_added event into the real ledger — a separate race from
    // locator propagation, see this function's own doc comment.
    hub_side::wait_for_signing_key_ledger_entry(&http, &node1, alice.identity_id).await;

    // Step 3: real cross-node login — Alice, who has never once talked to
    // Node 2, submits a grant signed with her Node-1-registered key
    // directly to Node 2. Node 2 has no local copy of her signing key, so
    // this only succeeds if it genuinely fetches and verifies it
    // cross-shard from Node 1 — the real proof this test exists for.
    let alice_token_on_node2 =
        hub_side::cross_node_login(&http, &node2, alice.identity_id, &alice_key).await;
    let alice_on_node2 = hub_side::Player {
        identity_id: alice.identity_id,
        token: alice_token_on_node2,
    };

    // Confirm it's a real, working session on Node 2, not just a token
    // that happens to parse — GET /me should already show her real
    // display name, resolved entirely from Node 2's own (freshly
    // provisioned) local state.
    let me_on_node2: serde_json::Value = http
        .get(format!("{node2}/me"))
        .bearer_auth(&alice_on_node2.token)
        .send()
        .await
        .expect("GET /me on node 2 failed")
        .json()
        .await
        .expect("GET /me on node 2 was not JSON");
    assert_eq!(
        me_on_node2["display_name"],
        format!("m3n-alice-{run_id}"),
        "node 2 should show Alice's real profile after a real cross-node login, not a stub"
    );

    // Step 4: with a real Node-2 session, Alice friends Bob and they form
    // a guild together — entirely on Node 2, both genuinely local there.
    hub_side::become_friends(&http, &node2, &alice_on_node2, &bob).await;
    let tag = format!("M{}", &run_id[..4]).to_uppercase();
    let guild_id = hub_side::create_guild(&http, &node2, &alice_on_node2, &tag).await;
    hub_side::join_guild_via_invite(&http, &node2, &alice_on_node2, &bob, guild_id).await;

    // Step 5: a game registers on Node 2 as well and issues Alice the
    // achievement, using her real Node-2 session.
    let game_slug = format!("m3n-game-{run_id}");
    let game = register_game_via_cli(&node2, &game_slug, "Milestone 1 (3-node) Game");
    define_dragon_slayer(&http, &node2, &game).await;
    hub_side::consent_to_integrator(
        &http,
        &node2,
        &alice_on_node2,
        &alice_key,
        &game.slug,
        &["achievements.issue", "achievements.read"],
    )
    .await;
    let alice_session_via_game =
        game_side::authenticate_player(&node2, &game, &alice_on_node2.token).await;
    let attestation_id = game_side::issue_dragon_slayer(&alice_session_via_game).await;
    let verified = game_side::verify_dragon_slayer(&alice_session_via_game).await;
    assert_eq!(verified.id, attestation_id);
    assert!(
        game_side::is_authentic(&verified),
        "dragon_slayer should be authentic"
    );
    assert!(
        game_side::is_valid(&verified),
        "dragon_slayer should be valid"
    );
    let recognition = game_side::game_recognizes(game.integrator_id, game.integrator_id, &verified);
    assert!(matches!(
        recognition,
        avalon_protocol::achievements::Recognition::Recognized
    ));

    // Step 6: the Hub view on Node 2 — a node Alice never registered on —
    // shows her friend, her guild, and her achievement. This is only
    // possible because of steps 2-5's real cross-node/cross-shard chain.
    let hub_view = hub_side::read_hub_view(&http, &node2, &alice_on_node2).await;
    assert_eq!(hub_view.display_name, format!("m3n-alice-{run_id}"));
    assert!(
        hub_view.friend_ids.contains(&bob.identity_id),
        "node 2's Hub view should show Bob as a friend"
    );
    assert!(
        hub_view.guild_ids.contains(&guild_id),
        "node 2's Hub view should show the guild"
    );
    assert!(
        hub_view
            .achievement_keys
            .iter()
            .any(|k| k.ends_with(":achievement:dragon_slayer")),
        "node 2's Hub view should show the Dragon Slayer achievement"
    );

    // Step 7: prove "any live node," not just a one-off pairing — Alice
    // also cross-node-logs into Node 3, a *third* real node that has
    // never seen her either, directly, without going through Node 2 at
    // all (a fresh grant straight from her own key against Node 1's
    // history again).
    hub_side::wait_for_locator_propagation(&http, &node3, &node1, alice.identity_id).await;
    let alice_token_on_node3 =
        hub_side::cross_node_login(&http, &node3, alice.identity_id, &alice_key).await;
    let me_on_node3: serde_json::Value = http
        .get(format!("{node3}/me"))
        .bearer_auth(&alice_token_on_node3)
        .send()
        .await
        .expect("GET /me on node 3 failed")
        .json()
        .await
        .expect("GET /me on node 3 was not JSON");
    assert_eq!(
        me_on_node3["display_name"],
        format!("m3n-alice-{run_id}"),
        "node 3 should also show Alice's real profile via its own independent cross-node login"
    );
}
