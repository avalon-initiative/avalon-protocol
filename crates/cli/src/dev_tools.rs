//! Everything that *mutates* state against a real `avalon-server` — creating
//! identities, logging in via a virtual (software-only) passkey, and
//! registering games — lives in this module, gated behind the `dev-tools`
//! Cargo feature (issue #173's second layer).
//!
//! `avalon inspect-ledger`/`inspect-ledger-full`/`outbox-status` (in
//! `main.rs`) stay outside this gate: they're read-only ops/diagnostic
//! commands, exactly the kind of thing an operator legitimately needs
//! against a real deployment. Everything in *this* file either drives a
//! software WebAuthn ceremony with no real hardware behind it
//! (`create_identity`, `login`) or performs an administrative write that, in
//! a real deployment, should be self-service by the actual user or game
//! developer through the real API — not an operator running a CLI on their
//! behalf (see `docs/architecture/settlement.md` and the discussion on
//! issue #173). `dev-tools` is on by default (this crate is exactly the
//! "local dev/ops CLI" its own top-level doc comment describes), but a build
//! meant to ship anywhere near a production deployment should be built with
//! `--no-default-features` — at which point this entire module, and every
//! dependency it alone pulls in (`rand`, `passkey-*`, `coset`, `reqwest`,
//! `url`), is compiled out of the binary completely, not merely hidden
//! behind a runtime check. `ed25519-dalek` itself is the one exception as
//! of issue #210 — `main.rs`'s always-available `inspect-ledger` now needs
//! `VerifyingKey` for signed-tree-head verification, so it's a required
//! dependency of this crate, not gated behind `dev-tools` alongside the
//! signing usage below.

use std::io::Write as _;
use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use coset::{CborSerializable, CoseKey};
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use passkey_types::Passkey;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub(crate) fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

/// Must produce exactly the bytes `avalon-server`'s
/// `handlers::identity_created_signing_bytes` reconstructs — see that
/// function's doc comment. Duplicated rather than shared: `avalon-cli`
/// doesn't depend on `avalon-server`.
fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
    format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
}

fn key_dir() -> PathBuf {
    PathBuf::from("_running/keys")
}

fn passkey_path(identity_id: Uuid) -> PathBuf {
    key_dir().join(format!("{identity_id}.passkey.json"))
}

/// A JSON-serializable mirror of `passkey_types::Passkey` (issue #115).
///
/// `Passkey` itself has no `Serialize`/`Deserialize` impl in this version of
/// `passkey-types` — its fields are all `pub`, so this converts field by
/// field instead of deriving through the real type. `key` (a `CoseKey`) is
/// serialized via `coset`'s own `CborSerializable::to_vec`/`from_slice` and
/// stored as base64 CBOR bytes, since `CoseKey` doesn't implement serde
/// either. `extensions` is never persisted — this CLI's virtual
/// authenticator never sets any, so `login` always reconstructs it as
/// `CredentialExtensions::default()`.
#[derive(Serialize, Deserialize)]
struct StoredPasskey {
    key_cbor_base64: String,
    credential_id_base64: String,
    rp_id: String,
    user_handle_base64: Option<String>,
    username: Option<String>,
    user_display_name: Option<String>,
    counter: Option<u32>,
}

impl From<&Passkey> for StoredPasskey {
    fn from(passkey: &Passkey) -> Self {
        Self {
            key_cbor_base64: BASE64.encode(
                passkey
                    .key
                    .clone()
                    .to_vec()
                    .expect("CoseKey should always serialize to CBOR"),
            ),
            credential_id_base64: BASE64.encode(Vec::from(passkey.credential_id.clone())),
            rp_id: passkey.rp_id.clone(),
            user_handle_base64: passkey
                .user_handle
                .clone()
                .map(|handle| BASE64.encode(Vec::from(handle))),
            username: passkey.username.clone(),
            user_display_name: passkey.user_display_name.clone(),
            counter: passkey.counter,
        }
    }
}

impl StoredPasskey {
    fn into_passkey(self) -> Passkey {
        let key_bytes = BASE64
            .decode(&self.key_cbor_base64)
            .expect("stored passkey's key_cbor_base64 should be valid base64");
        Passkey {
            key: CoseKey::from_slice(&key_bytes)
                .expect("stored passkey's CoseKey CBOR should decode"),
            credential_id: BASE64
                .decode(&self.credential_id_base64)
                .expect("stored passkey's credential_id_base64 should be valid base64")
                .into(),
            rp_id: self.rp_id,
            user_handle: self.user_handle_base64.map(|encoded| {
                BASE64
                    .decode(encoded)
                    .expect("stored passkey's user_handle_base64 should be valid base64")
                    .into()
            }),
            username: self.username,
            user_display_name: self.user_display_name,
            counter: self.counter,
            extensions: Default::default(),
        }
    }
}

pub(crate) async fn create_identity() {
    print!("Display name: ");
    std::io::stdout().flush().ok();
    let mut display_name = String::new();
    std::io::stdin()
        .read_line(&mut display_name)
        .expect("failed to read display name");
    let display_name = display_name.trim().to_string();

    let identity_id = Uuid::new_v4();
    let base = server_url();
    let http = reqwest::Client::new();

    // The identity's event-signing key (see docs/architecture/identity.md
    // and crates/server/src/auth.rs) — separate from the WebAuthn passkey
    // below, and the only thing that signs `identity.created`.
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let event_signing_public_key = BASE64.encode(signing_key.verifying_key().to_bytes());

    // A software/virtual WebAuthn authenticator — no browser, no hardware.
    // This is exactly the intended use of the `testable` feature: it drives
    // the same standard ceremony a real browser+passkey would, against the
    // real HTTP endpoints below.
    let store = MemoryStore::new();
    let user_mock = MockUserValidationMethod::verified_user(1);
    let authenticator = Authenticator::new(Aaguid::new_empty(), store, user_mock);
    let mut client = Client::new(authenticator).allows_insecure_localhost(true);
    let origin_str = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let origin_url =
        url::Url::parse(&origin_str).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL");

    let start_body = json!({ "identity_id": identity_id, "display_name": display_name });
    let start: serde_json::Value = http
        .post(format!("{base}/identities/register/start"))
        .json(&start_body)
        .send()
        .await
        .expect("register/start request failed — is `make start` running?")
        .json()
        .await
        .expect("register/start response was not JSON");

    let ticket_id = start["ticket_id"]
        .as_str()
        .expect("register/start response missing ticket_id")
        .to_string();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone())
            .expect("failed to parse WebAuthn creation challenge");

    let webauthn_credential = client
        .register(
            Origin::from(&origin_url),
            creation_options,
            DefaultClientData,
        )
        .await
        .expect("WebAuthn registration ceremony failed");

    let signing_bytes = identity_created_signing_bytes(identity_id, &display_name);
    let signature = signing_key.sign(&signing_bytes);
    let event_signature = BASE64.encode(signature.to_bytes());

    let finish_body = json!({
        "ticket_id": ticket_id,
        "webauthn_credential": webauthn_credential,
        "event_signing_public_key": event_signing_public_key,
        "event_signature": event_signature,
    });
    let finish_response = http
        .post(format!("{base}/identities/register/finish"))
        .json(&finish_body)
        .send()
        .await
        .expect("register/finish request failed");
    if !finish_response.status().is_success() {
        eprintln!(
            "registration failed: {:?}\n{}",
            finish_response.status(),
            finish_response.text().await.unwrap_or_default()
        );
        std::process::exit(1);
    }

    // The event-signing key: nothing in this milestone lets a player carry
    // it between sessions except a local file, so it's saved here for later
    // reuse (e.g. a future `profile.updated`-signing command, issue #86).
    let key_dir = key_dir();
    std::fs::create_dir_all(&key_dir).ok();
    let key_path = key_dir.join(format!("{identity_id}.signing-key"));
    std::fs::write(&key_path, BASE64.encode(signing_key.to_bytes()))
        .expect("failed to write signing key file");

    // The passkey (issue #115): on a real device this lives with the
    // platform authenticator and this CLI would never touch it — but this
    // CLI *is* the device here (a virtual, in-process one, gone the moment
    // this process exits), so `avalon login` has no way to "ask the same
    // device again" unless something plays that role. Persisting the one
    // credential this registration just created is that stand-in — the
    // closest honest equivalent to "the device remembers its passkey."
    let store = client.authenticator().store();
    let passkey = store
        .values()
        .next()
        .expect("registration should have saved exactly one passkey to the virtual store");
    let stored_passkey = StoredPasskey::from(passkey);
    let passkey_path = passkey_path(identity_id);
    std::fs::write(
        &passkey_path,
        serde_json::to_string(&stored_passkey).expect("StoredPasskey should serialize"),
    )
    .expect("failed to write passkey file");

    println!();
    println!("Identity created: {identity_id}");
    println!("Event-signing key saved to: {}", key_path.display());
    println!("Passkey saved to:            {}", passkey_path.display());
    println!();
    println!("Log back in from this machine with:");
    println!("  avalon login {identity_id}");
    println!();
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ WARNING                                                      │");
    println!("│ Your passkey IS your identity. If every device holding it is │");
    println!("│ lost, this identity and everything durable attached to it —  │");
    println!("│ friends, guilds, achievements, history — is gone forever.    │");
    println!("│ There is no recovery mechanism yet (issue #99). Register a   │");
    println!("│ second passkey from another device once that's supported.   │");
    println!("└─────────────────────────────────────────────────────────────┘");
}

/// `avalon login <identity_id>` (issue #115) — a dev/test convenience, not a
/// pattern for any real deployment: this drives a genuine
/// `/sessions/start` → `/sessions/finish` WebAuthn ceremony, the same as a
/// browser would, but prints the resulting bearer token straight to the
/// terminal. Fails clearly (never silently registers a new passkey) if
/// `identity_id` has nothing saved locally — see `create_identity`'s
/// `StoredPasskey` persistence, the only source this can authenticate
/// against.
pub(crate) async fn login(identity_id: Uuid) {
    let passkey_path = passkey_path(identity_id);
    let stored_json = std::fs::read_to_string(&passkey_path).unwrap_or_else(|_| {
        eprintln!(
            "no locally-saved passkey for {identity_id} (looked in {}).",
            passkey_path.display()
        );
        eprintln!("this identity wasn't created on this machine — run `avalon create-identity` first, or log in from the machine that did.");
        std::process::exit(1);
    });
    let stored: StoredPasskey = serde_json::from_str(&stored_json)
        .expect("saved passkey file was not valid StoredPasskey JSON");
    let passkey = stored.into_passkey();

    let base = server_url();
    let http = reqwest::Client::new();
    let origin_str = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let origin_url =
        url::Url::parse(&origin_str).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL");

    // Rebuild a fresh virtual authenticator around exactly the one
    // credential this identity registered — "the same device asking again."
    let mut store = MemoryStore::new();
    store.insert(Vec::from(passkey.credential_id.clone()), passkey);
    let user_mock = MockUserValidationMethod::verified_user(1);
    let authenticator = Authenticator::new(Aaguid::new_empty(), store, user_mock);
    let mut client = Client::new(authenticator).allows_insecure_localhost(true);

    let start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&json!({ "identity_id": identity_id }))
        .send()
        .await
        .expect("sessions/start request failed — is `make start` running?")
        .json()
        .await
        .expect("sessions/start response was not JSON");
    let ticket_id = start["ticket_id"]
        .as_str()
        .expect("sessions/start response missing ticket_id")
        .to_string();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(start["challenge"].clone())
            .expect("failed to parse WebAuthn request challenge");

    let assertion = client
        .authenticate(
            Origin::from(&origin_url),
            request_options,
            DefaultClientData,
        )
        .await
        .expect("WebAuthn authentication ceremony failed");

    let finish_response = http
        .post(format!("{base}/sessions/finish"))
        .json(&json!({ "ticket_id": ticket_id, "credential": assertion }))
        .send()
        .await
        .expect("sessions/finish request failed");
    if !finish_response.status().is_success() {
        eprintln!(
            "login failed: {:?}\n{}",
            finish_response.status(),
            finish_response.text().await.unwrap_or_default()
        );
        std::process::exit(1);
    }
    let finish_body: serde_json::Value = finish_response
        .json()
        .await
        .expect("sessions/finish response was not JSON");
    let token = finish_body["token"]
        .as_str()
        .expect("sessions/finish response missing token");
    let expires_at = finish_body["expires_at"]
        .as_str()
        .expect("sessions/finish response missing expires_at");

    println!();
    println!("Logged in: {identity_id}");
    println!("Token:      {token}");
    println!("Expires at: {expires_at}");
    println!();
    println!("This is a live bearer token — fine to print for local dev against a");
    println!("throwaway database, not a pattern to carry into any real deployment.");
}

pub(crate) const REGISTER_GAME_USAGE: &str = "usage: avalon register-game --slug <slug> --name <name> --developer <dev> [--capability <cap>]... [--server <url>]";

/// Parsed `avalon register-game` arguments. Hand-rolled to match this file's
/// existing `match command.as_deref()` style rather than pulling in `clap`
/// (not already a dependency of this crate).
#[derive(Debug)]
pub(crate) struct RegisterGameArgs {
    slug: String,
    name: String,
    developer: String,
    capabilities: Vec<String>,
    server: Option<String>,
}

impl RegisterGameArgs {
    pub(crate) fn parse(args: &[String]) -> Result<Self, String> {
        let mut slug = None;
        let mut name = None;
        let mut developer = None;
        let mut capabilities = Vec::new();
        let mut server = None;

        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--slug" => slug = Some(iter.next().ok_or("--slug requires a value")?.clone()),
                "--name" => name = Some(iter.next().ok_or("--name requires a value")?.clone()),
                "--developer" => {
                    developer = Some(iter.next().ok_or("--developer requires a value")?.clone())
                }
                "--capability" => {
                    capabilities.push(iter.next().ok_or("--capability requires a value")?.clone())
                }
                "--server" => {
                    server = Some(iter.next().ok_or("--server requires a value")?.clone())
                }
                other => return Err(format!("unrecognized argument: {other}")),
            }
        }

        Ok(Self {
            slug: slug.ok_or("--slug is required")?,
            name: name.ok_or("--name is required")?,
            developer: developer.ok_or("--developer is required")?,
            capabilities,
            server,
        })
    }
}

/// `avalon register-game` (issue #29) — registers a test game against #26's
/// `POST /games`, generating a fresh Ed25519 signing keypair locally (the
/// only algorithm `crate::auth::verify_event_signature` on the server side
/// can verify — see `crates/server/src/games.rs`). Only the public key is
/// ever sent to the server; the private key is saved locally (mirroring
/// `create_identity`'s event-signing-key persistence) and printed exactly
/// once, since the server never stores or returns it again.
pub(crate) async fn register_game(args: RegisterGameArgs) {
    let base = args.server.clone().unwrap_or_else(server_url);
    let http = reqwest::Client::new();

    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let public_key_base64 = BASE64.encode(signing_key.verifying_key().to_bytes());
    let private_key_base64 = BASE64.encode(signing_key.to_bytes());

    let request_body = json!({
        "slug": args.slug,
        "name": args.name,
        "developer": args.developer,
        "requested_capabilities": args.capabilities,
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": public_key_base64,
        },
    });

    let response = http
        .post(format!("{base}/games"))
        .json(&request_body)
        .send()
        .await
        .expect("POST /games request failed — is `make start` running?");

    if response.status() == reqwest::StatusCode::CONFLICT {
        eprintln!(
            "game registration failed: slug '{}' is already taken.",
            args.slug
        );
        eprintln!("slugs are forever and can't be renamed or reused — pick a different --slug.");
        std::process::exit(1);
    }
    if !response.status().is_success() {
        eprintln!(
            "game registration failed: {:?}\n{}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        std::process::exit(1);
    }

    let response_body: serde_json::Value = response
        .json()
        .await
        .expect("POST /games response was not JSON");
    let game_id = response_body["id"]
        .as_str()
        .expect("POST /games response missing id")
        .to_string();
    let key_id = response_body["credential"]["key_id"]
        .as_str()
        .expect("POST /games response missing credential.key_id")
        .to_string();

    // The game's signing key: nothing else lets this CLI reuse it later
    // (e.g. for the challenge-response sanity check just below, or a future
    // `avalon` command acting as this game) except a local file — same
    // rationale as `create_identity`'s event-signing-key persistence.
    let key_dir = key_dir();
    std::fs::create_dir_all(&key_dir).ok();
    let key_path = key_dir.join(format!("game-{}.signing-key", args.slug));
    std::fs::write(&key_path, &private_key_base64).expect("failed to write signing key file");

    println!();
    println!("Game registered: {} ({game_id})", args.slug);
    println!("Key ID:               {key_id}");
    println!("Signing key saved to: {}", key_path.display());
    println!();
    println!("Private signing key (base64):");
    println!("  {private_key_base64}");
    println!();
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ WARNING                                                      │");
    println!("│ This is the ONLY time this private signing key is shown.    │");
    println!("│ Avalon only ever stores the public key — if this key is     │");
    println!(
        "│ lost, '{:<12}' can no longer authenticate as this game    │",
        args.slug
    );
    println!("│ and there is no recovery; register a new key/game instead.  │");
    println!("└─────────────────────────────────────────────────────────────┘");

    match register_game_auth_sanity_check(&http, &base, &args.slug, &key_id, &signing_key).await {
        Ok(whoami_game_id) => {
            println!();
            println!("Challenge-response sanity check passed (whoami: {whoami_game_id}).");
        }
        Err(message) => {
            println!();
            println!(
                "Challenge-response sanity check failed ({message}) — registration itself succeeded."
            );
        }
    }
}

/// Exercises the challenge-response round trip #26 built
/// (`crates/server/src/games.rs`'s `create_game_challenge`/
/// `authenticate_game`) once, as a sanity check that the freshly registered
/// key actually works end to end. Not load-bearing for registration itself —
/// any failure here is reported but doesn't fail the command.
async fn register_game_auth_sanity_check(
    http: &reqwest::Client,
    base: &str,
    slug: &str,
    key_id: &str,
    signing_key: &SigningKey,
) -> Result<String, String> {
    let challenge: serde_json::Value = http
        .post(format!("{base}/games/{slug}/challenge"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let challenge_id = challenge["challenge_id"]
        .as_str()
        .ok_or("challenge response missing challenge_id")?;
    let nonce = BASE64
        .decode(
            challenge["nonce"]
                .as_str()
                .ok_or("challenge response missing nonce")?,
        )
        .map_err(|e| e.to_string())?;

    let signature = signing_key.sign(&nonce);

    let response = http
        .get(format!("{base}/games/whoami"))
        .header("x-avalon-game-key-id", key_id)
        .header("x-avalon-game-challenge-id", challenge_id)
        .header(
            "x-avalon-game-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("whoami returned {}", response.status()));
    }
    let whoami: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(whoami["game_id"]
        .as_str()
        .ok_or("whoami response missing game_id")?
        .to_string())
}

#[cfg(test)]
mod tests {
    //! `StoredPasskey`'s round-trip is the one piece of #115 worth unit
    //! testing directly — everything else needs a real server. Uses
    //! `Passkey::mock(...)` (the `testable` feature's own test-fixture
    //! builder — already enabled workspace-wide via `passkey-authenticator`)
    //! rather than a real registration ceremony.

    use super::*;

    #[test]
    fn stored_passkey_round_trips_every_field() {
        let original = Passkey::mock("localhost".to_string())
            .username("alice".to_string())
            .user_display_name("Alice".to_string())
            .user_handle(None)
            .counter(3)
            .build();

        let stored = StoredPasskey::from(&original);
        let json = serde_json::to_string(&stored).expect("StoredPasskey should serialize");
        let reloaded: StoredPasskey =
            serde_json::from_str(&json).expect("StoredPasskey should deserialize");
        let reconstructed = reloaded.into_passkey();

        assert_eq!(
            reconstructed.key.clone().to_vec().unwrap(),
            original.key.clone().to_vec().unwrap()
        );
        assert_eq!(reconstructed.credential_id, original.credential_id);
        assert_eq!(reconstructed.rp_id, original.rp_id);
        assert_eq!(reconstructed.user_handle, original.user_handle);
        assert_eq!(reconstructed.username, original.username);
        assert_eq!(reconstructed.user_display_name, original.user_display_name);
        assert_eq!(reconstructed.counter, original.counter);
    }

    #[test]
    fn stored_passkey_round_trips_a_present_user_handle() {
        let original = Passkey::mock("localhost".to_string())
            .user_handle(Some(16))
            .build();

        let stored = StoredPasskey::from(&original);
        let reconstructed: Passkey = serde_json::from_str(
            &serde_json::to_string(&stored).expect("StoredPasskey should serialize"),
        )
        .map(StoredPasskey::into_passkey)
        .expect("StoredPasskey should deserialize");

        assert_eq!(reconstructed.user_handle, original.user_handle);
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn register_game_args_parses_required_fields() {
        let parsed = RegisterGameArgs::parse(&args(&[
            "--slug",
            "ashen-realms",
            "--name",
            "Ashen Realms",
            "--developer",
            "Ashen Studios",
        ]))
        .expect("should parse with only required args");

        assert_eq!(parsed.slug, "ashen-realms");
        assert_eq!(parsed.name, "Ashen Realms");
        assert_eq!(parsed.developer, "Ashen Studios");
        assert!(parsed.capabilities.is_empty());
        assert_eq!(parsed.server, None);
    }

    #[test]
    fn register_game_args_collects_repeated_capability_flags() {
        let parsed = RegisterGameArgs::parse(&args(&[
            "--slug",
            "ashen-realms",
            "--name",
            "Ashen Realms",
            "--developer",
            "Ashen Studios",
            "--capability",
            "friends.read",
            "--capability",
            "achievements.write",
            "--server",
            "http://example.test",
        ]))
        .expect("should parse with repeated --capability flags");

        assert_eq!(
            parsed.capabilities,
            vec!["friends.read".to_string(), "achievements.write".to_string()]
        );
        assert_eq!(parsed.server.as_deref(), Some("http://example.test"));
    }

    #[test]
    fn register_game_args_rejects_missing_required_args() {
        let err = RegisterGameArgs::parse(&args(&["--slug", "ashen-realms"]))
            .expect_err("missing --name and --developer should fail to parse");
        assert!(err.contains("--name"));
    }

    #[test]
    fn register_game_args_rejects_flag_missing_its_value() {
        let err = RegisterGameArgs::parse(&args(&["--slug"]))
            .expect_err("a trailing flag with no value should fail to parse");
        assert!(err.contains("--slug"));
    }

    #[test]
    fn register_game_args_rejects_unrecognized_flags() {
        let err = RegisterGameArgs::parse(&args(&[
            "--slug",
            "ashen-realms",
            "--name",
            "Ashen Realms",
            "--developer",
            "Ashen Studios",
            "--bogus",
            "value",
        ]))
        .expect_err("an unrecognized flag should fail to parse");
        assert!(err.contains("--bogus"));
    }
}
