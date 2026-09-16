//! Everything that *mutates* state against a real `avalon-server` — creating
//! identities, logging in via a virtual (software-only) passkey, and
//! registering integrators — lives in this module, gated behind the `dev-tools`
//! Cargo feature (issue #173's second layer).
//!
//! `avalon inspect-ledger`/`inspect-ledger-full`/`outbox-status` (in
//! `main.rs`) stay outside this gate: they're read-only ops/diagnostic
//! commands, exactly the kind of thing an operator legitimately needs
//! against a real deployment. Everything in *this* file either drives a
//! software WebAuthn ceremony with no real hardware behind it
//! (`create_identity`, `login`) or performs an administrative write that, in
//! a real deployment, should be self-service by the actual user or integrator
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

    // The event-signing key: nothing in this milestone lets a user carry
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

/// `avalon pair-device` (issue #307) — drives the `start`/`poll` side of
/// cross-device pairing, standing in for a real WebAuthn-incapable client
/// (a console, a headless game engine) so the flow is testable end to end
/// without one. Prints the `user_code` for a user to enter on the Hub's
/// pairing page, then polls until the pairing is approved, denied, or
/// expires.
pub(crate) async fn pair_device() {
    let base = server_url();
    let http = reqwest::Client::new();

    let start: serde_json::Value = http
        .post(format!("{base}/auth/device/start"))
        .send()
        .await
        .expect("device/start request failed — is `make start` running?")
        .json()
        .await
        .expect("device/start response was not JSON");

    let device_code = start["device_code"]
        .as_str()
        .expect("device/start response missing device_code")
        .to_string();
    let user_code = start["user_code"]
        .as_str()
        .expect("device/start response missing user_code");
    let verification_uri = start["verification_uri"]
        .as_str()
        .expect("device/start response missing verification_uri");
    let poll_interval_secs = start["poll_interval"].as_u64().unwrap_or(5);

    println!();
    println!("Go to: {verification_uri}");
    println!("Enter code: {user_code}");
    println!();
    println!("Waiting for approval...");

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;

        let poll: serde_json::Value = http
            .post(format!("{base}/auth/device/poll"))
            .bearer_auth(&device_code)
            .send()
            .await
            .expect("device/poll request failed")
            .json()
            .await
            .expect("device/poll response was not JSON");

        match poll["status"].as_str().unwrap_or("") {
            "pending" | "slow_down" => continue,
            "approved" => {
                let token = poll["token"]
                    .as_str()
                    .expect("approved poll response missing token");
                let expires_at = poll["expires_at"]
                    .as_str()
                    .expect("approved poll response missing expires_at");
                println!();
                println!("Paired.");
                println!("Token:      {token}");
                println!("Expires at: {expires_at}");
                return;
            }
            "denied" => {
                eprintln!("pairing was denied.");
                std::process::exit(1);
            }
            other => {
                eprintln!("pairing {other} — request a new code with `avalon pair-device`.");
                std::process::exit(1);
            }
        }
    }
}

/// What `register_integrator` saves alongside the raw private-key file, so
/// `issue_achievement` (issue #48) can resolve `--integrator <slug>` to a
/// `key_id` without the operator having to paste it back in from
/// registration's one-time printout.
#[derive(Serialize, Deserialize)]
struct IntegratorCredentialsFile {
    #[allow(dead_code)]
    integrator_id: String,
    key_id: String,
}

pub(crate) const REGISTER_INTEGRATOR_USAGE: &str = "usage: avalon register-integrator --slug <slug> --name <name> --owner-name <owner> [--capability <cap>]... [--server <url>]";

/// Parsed `avalon register-integrator` arguments. Hand-rolled to match this file's
/// existing `match command.as_deref()` style rather than pulling in `clap`
/// (not already a dependency of this crate).
#[derive(Debug)]
pub(crate) struct RegisterIntegratorArgs {
    slug: String,
    name: String,
    owner_name: String,
    capabilities: Vec<String>,
    server: Option<String>,
}

impl RegisterIntegratorArgs {
    pub(crate) fn parse(args: &[String]) -> Result<Self, String> {
        let mut slug = None;
        let mut name = None;
        let mut owner_name = None;
        let mut capabilities = Vec::new();
        let mut server = None;

        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--slug" => slug = Some(iter.next().ok_or("--slug requires a value")?.clone()),
                "--name" => name = Some(iter.next().ok_or("--name requires a value")?.clone()),
                // `--developer` is the original spelling (#29), kept working
                // as an alias so existing scripts don't break.
                "--owner-name" | "--developer" => {
                    owner_name = Some(iter.next().ok_or("--owner-name requires a value")?.clone())
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
            owner_name: owner_name.ok_or("--owner-name is required")?,
            capabilities,
            server,
        })
    }
}

/// `avalon register-integrator` (issue #29) — registers a test integrator against #26's
/// registration endpoint, sent to `POST /integrations` (#293's canonical
/// alias for the same `POST /integrations` handler — a `register-integrator`
/// command alias is a separate, lower-priority follow-up, not this),
/// generating a fresh Ed25519 signing keypair locally (the
/// only algorithm `crate::auth::verify_event_signature` on the server side
/// can verify — see `crates/server/src/integrations.rs`). Only the public key is
/// ever sent to the server; the private key is saved locally (mirroring
/// `create_identity`'s event-signing-key persistence) and printed exactly
/// once, since the server never stores or returns it again.
pub(crate) async fn register_integrator(args: RegisterIntegratorArgs) {
    let base = args.server.clone().unwrap_or_else(server_url);
    let http = reqwest::Client::new();

    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let public_key_base64 = BASE64.encode(signing_key.verifying_key().to_bytes());
    let private_key_base64 = BASE64.encode(signing_key.to_bytes());

    let request_body = json!({
        "slug": args.slug,
        "name": args.name,
        "owner_name": args.owner_name,
        "requested_capabilities": args.capabilities,
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": public_key_base64,
        },
    });

    let response = http
        .post(format!("{base}/integrations"))
        .json(&request_body)
        .send()
        .await
        .expect("POST /integrations request failed — is `make start` running?");

    if response.status() == reqwest::StatusCode::CONFLICT {
        eprintln!(
            "integrator registration failed: slug '{}' is already taken.",
            args.slug
        );
        eprintln!("slugs are forever and can't be renamed or reused — pick a different --slug.");
        std::process::exit(1);
    }
    if !response.status().is_success() {
        eprintln!(
            "integrator registration failed: {:?}\n{}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        std::process::exit(1);
    }

    let response_body: serde_json::Value = response
        .json()
        .await
        .expect("POST /integrations response was not JSON");
    let integrator_id = response_body["id"]
        .as_str()
        .expect("POST /integrations response missing id")
        .to_string();
    let key_id = response_body["credential"]["key_id"]
        .as_str()
        .expect("POST /integrations response missing credential.key_id")
        .to_string();

    // The integrator's signing key: nothing else lets this CLI reuse it later
    // (e.g. for the challenge-response sanity check just below, or a future
    // `avalon` command acting as this integrator) except a local file — same
    // rationale as `create_identity`'s event-signing-key persistence.
    let key_dir = key_dir();
    std::fs::create_dir_all(&key_dir).ok();
    let key_path = key_dir.join(format!("integrator-{}.signing-key", args.slug));
    std::fs::write(&key_path, &private_key_base64).expect("failed to write signing key file");

    // `key_id` itself (as opposed to the private key) is otherwise only
    // ever printed once, below — save it too (issue #48's own
    // `issue-achievement` needs it) so a later command can look it up by
    // `--integrator <slug>` alone instead of requiring the operator to
    // paste it back in. See `load_integrator_credentials`.
    let credentials_path = key_dir.join(format!("integrator-{}.json", args.slug));
    std::fs::write(
        &credentials_path,
        serde_json::to_string(&IntegratorCredentialsFile {
            integrator_id: integrator_id.clone(),
            key_id: key_id.clone(),
        })
        .expect("IntegratorCredentialsFile should serialize"),
    )
    .expect("failed to write integrator credentials file");

    println!();
    println!("Integrator registered: {} ({integrator_id})", args.slug);
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
        "│ lost, '{:<12}' can no longer authenticate as this integrator    │",
        args.slug
    );
    println!("│ and there is no recovery; register a new key/integrator instead.  │");
    println!("└─────────────────────────────────────────────────────────────┘");

    match register_integrator_auth_sanity_check(&http, &base, &args.slug, &key_id, &signing_key)
        .await
    {
        Ok(whoami_integrator_id) => {
            println!();
            println!("Challenge-response sanity check passed (whoami: {whoami_integrator_id}).");
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
/// (`crates/server/src/integrations.rs`'s `create_integrator_challenge`/
/// `authenticate_integrator`) once, as a sanity check that the freshly registered
/// key actually works end to end. Not load-bearing for registration itself —
/// any failure here is reported but doesn't fail the command.
async fn register_integrator_auth_sanity_check(
    http: &reqwest::Client,
    base: &str,
    slug: &str,
    key_id: &str,
    signing_key: &SigningKey,
) -> Result<String, String> {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{slug}/challenge"))
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
        .get(format!("{base}/integrations/whoami"))
        .header("x-avalon-integrator-key-id", key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("whoami returned {}", response.status()));
    }
    let whoami: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(whoami["integrator_id"]
        .as_str()
        .ok_or("whoami response missing integrator_id")?
        .to_string())
}

pub(crate) const ISSUE_ACHIEVEMENT_USAGE: &str = "usage: avalon issue-achievement --integrator <slug> --achievement <key> --token <session-token> [--key <path>] [--key-id <uuid>] [--server <url>]";

/// Parsed `avalon issue-achievement` arguments — same hand-rolled style as
/// [`RegisterIntegratorArgs`]. `--token` is a session bearer token for the
/// identity the achievement is issued *to*; this CLI never creates or
/// chooses that identity itself, matching `avalon_sdk::Session::issue_achievement`'s
/// own "issues to the session's own identity" design (see that method's
/// doc comment) — get one with `avalon login <identity_id>` first. `--key`
/// and `--key-id` default to what `register_integrator` saved for
/// `--integrator`; pass them explicitly for an integrator registered
/// elsewhere (or before this file started saving the sidecar credentials
/// file).
#[derive(Debug)]
pub(crate) struct IssueAchievementArgs {
    integrator: String,
    achievement: String,
    token: String,
    key_path: Option<String>,
    key_id: Option<String>,
    server: Option<String>,
}

impl IssueAchievementArgs {
    pub(crate) fn parse(args: &[String]) -> Result<Self, String> {
        let mut integrator = None;
        let mut achievement = None;
        let mut token = None;
        let mut key_path = None;
        let mut key_id = None;
        let mut server = None;

        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--integrator" => {
                    integrator = Some(iter.next().ok_or("--integrator requires a value")?.clone())
                }
                "--achievement" => {
                    achievement = Some(iter.next().ok_or("--achievement requires a value")?.clone())
                }
                "--token" => token = Some(iter.next().ok_or("--token requires a value")?.clone()),
                "--key" => key_path = Some(iter.next().ok_or("--key requires a value")?.clone()),
                "--key-id" => {
                    key_id = Some(iter.next().ok_or("--key-id requires a value")?.clone())
                }
                "--server" => {
                    server = Some(iter.next().ok_or("--server requires a value")?.clone())
                }
                other => return Err(format!("unrecognized argument: {other}")),
            }
        }

        Ok(Self {
            integrator: integrator.ok_or("--integrator is required")?,
            achievement: achievement.ok_or("--achievement is required")?,
            token: token.ok_or("--token is required")?,
            key_path,
            key_id,
            server,
        })
    }
}

/// `avalon issue-achievement` (issue #48) — issues `--achievement` (already
/// defined against `--integrator` via `POST /integrations/{slug}/achievements`,
/// a step this command doesn't do itself) to the identity behind `--token`,
/// through `avalon-sdk` exactly the way a real game/app/service would
/// (`Session::issue_achievement`, #34) — never a raw HTTP request built by
/// hand, per this ticket's own "developer-facing commands go through the
/// SDK/API" invariant. Resolves `--key`/`--key-id` from what
/// `register_integrator` saved for `--integrator` when not given
/// explicitly.
pub(crate) async fn issue_achievement(args: IssueAchievementArgs) {
    let base = args.server.clone().unwrap_or_else(server_url);

    let key_path = args
        .key_path
        .map(PathBuf::from)
        .unwrap_or_else(|| key_dir().join(format!("integrator-{}.signing-key", args.integrator)));
    let key_base64 = std::fs::read_to_string(&key_path).unwrap_or_else(|_| {
        eprintln!(
            "no signing key found at {} — pass --key <path>, or run `avalon register-integrator` first.",
            key_path.display()
        );
        std::process::exit(1);
    });
    let key_bytes: [u8; 32] = BASE64
        .decode(key_base64.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or_else(|| {
            eprintln!(
                "{} did not contain a valid base64-encoded 32-byte Ed25519 key.",
                key_path.display()
            );
            std::process::exit(1);
        });

    let key_id = match args.key_id {
        Some(key_id) => key_id,
        None => {
            let credentials_path = key_dir().join(format!("integrator-{}.json", args.integrator));
            let credentials_json = std::fs::read_to_string(&credentials_path).unwrap_or_else(|_| {
                eprintln!(
                    "no saved credentials at {} — pass --key-id <uuid> explicitly, or run `avalon register-integrator` first.",
                    credentials_path.display()
                );
                std::process::exit(1);
            });
            let credentials: IntegratorCredentialsFile = serde_json::from_str(&credentials_json)
                .expect("saved integrator credentials file was not valid JSON");
            credentials.key_id
        }
    };

    let client = avalon_sdk::AvalonClient::new(avalon_sdk::AvalonConfig {
        server_url: base,
        integrator_credential_key_id: key_id,
        integrator_slug: Some(args.integrator.clone()),
        signing_key: Some(key_bytes),
        retry: Default::default(),
    });

    let session = client.authenticate(&args.token).await.unwrap_or_else(|e| {
        eprintln!("authenticate() failed: {e}");
        eprintln!("is --token a valid, unexpired session token? get one with `avalon login <identity_id>`.");
        std::process::exit(1);
    });

    match session.issue_achievement(&args.achievement).await {
        Ok(attestation_id) => {
            println!();
            println!(
                "Issued '{}' (integrator: {}) to identity {}.",
                args.achievement,
                args.integrator,
                session.identity().id.0
            );
            println!("Attestation id: {attestation_id}");
        }
        Err(e) => {
            eprintln!("issue_achievement failed: {e}");
            std::process::exit(1);
        }
    }
}

pub(crate) const REGISTER_ISSUER_USAGE: &str = "usage: avalon register-issuer --integrator <slug> (--network-id <network_id> | --env <dev|int|mainnet>) [--issuer-ref <ref>] [--key <path>] [--server <url>]";

/// Parsed `avalon register-issuer` arguments — same hand-rolled style as
/// [`RegisterIntegratorArgs`]/[`IssueAchievementArgs`]. Exactly one of
/// `--network-id`/`--env` is required (#483's own "no implicit default"
/// invariant) — `avalon_sdk::network::TargetNetwork` has no default
/// variant to fall back to, and this parser doesn't invent one either.
#[derive(Debug)]
pub(crate) struct RegisterIssuerArgs {
    integrator: String,
    issuer_ref: Option<String>,
    network_id: Option<String>,
    env: Option<String>,
    key_path: Option<String>,
    server: Option<String>,
}

impl RegisterIssuerArgs {
    pub(crate) fn parse(args: &[String]) -> Result<Self, String> {
        let mut integrator = None;
        let mut issuer_ref = None;
        let mut network_id = None;
        let mut env = None;
        let mut key_path = None;
        let mut server = None;

        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--integrator" => {
                    integrator = Some(iter.next().ok_or("--integrator requires a value")?.clone())
                }
                "--issuer-ref" => {
                    issuer_ref = Some(iter.next().ok_or("--issuer-ref requires a value")?.clone())
                }
                "--network-id" => {
                    network_id = Some(iter.next().ok_or("--network-id requires a value")?.clone())
                }
                "--env" => env = Some(iter.next().ok_or("--env requires a value")?.clone()),
                "--key" => key_path = Some(iter.next().ok_or("--key requires a value")?.clone()),
                "--server" => {
                    server = Some(iter.next().ok_or("--server requires a value")?.clone())
                }
                other => return Err(format!("unrecognized argument: {other}")),
            }
        }

        if network_id.is_some() == env.is_some() {
            return Err("exactly one of --network-id or --env is required".to_string());
        }

        Ok(Self {
            integrator: integrator.ok_or("--integrator is required")?,
            issuer_ref,
            network_id,
            env,
            key_path,
            server,
        })
    }

    fn target_network(&self) -> Result<avalon_sdk::network::TargetNetwork, String> {
        if let Some(network_id) = &self.network_id {
            return Ok(avalon_sdk::network::TargetNetwork::NetworkId(
                network_id.clone(),
            ));
        }
        let env = self
            .env
            .as_deref()
            .expect("parse() enforces exactly one of --network-id/--env");
        let tier = match env.to_ascii_lowercase().as_str() {
            "dev" => avalon_sdk::network::TargetNetworkTier::Dev,
            "int" => avalon_sdk::network::TargetNetworkTier::Int,
            "mainnet" => avalon_sdk::network::TargetNetworkTier::Mainnet,
            other => {
                return Err(format!(
                    "unrecognized --env '{other}' (expected dev, int, or mainnet)"
                ))
            }
        };
        Ok(avalon_sdk::network::TargetNetwork::Env(tier))
    }
}

/// `avalon register-issuer` (issue #483, on top of #481's endpoint) —
/// registers `--integrator`'s signing key as an issuer on the network it
/// declares intent for, through `avalon_sdk::AvalonClient::register_issuer`
/// exactly the way a real game/app/service would — never a raw HTTP
/// request built by hand, matching `issue_achievement`'s own "developer-
/// facing commands go through the SDK" invariant. Resolves `--key` from
/// what `register_integrator` saved for `--integrator` when not given
/// explicitly, same as `issue_achievement`. `--issuer-ref` defaults to
/// `game:<integrator>`, matching `Session::issue_achievement`'s own
/// convention.
pub(crate) async fn register_issuer(args: RegisterIssuerArgs) {
    let target = args.target_network().unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(1);
    });
    let base = args.server.clone().unwrap_or_else(server_url);

    let key_path =
        args.key_path.clone().map(PathBuf::from).unwrap_or_else(|| {
            key_dir().join(format!("integrator-{}.signing-key", args.integrator))
        });
    let key_base64 = std::fs::read_to_string(&key_path).unwrap_or_else(|_| {
        eprintln!(
            "no signing key found at {} — pass --key <path>, or run `avalon register-integrator` first.",
            key_path.display()
        );
        std::process::exit(1);
    });
    let key_bytes: [u8; 32] = BASE64
        .decode(key_base64.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or_else(|| {
            eprintln!(
                "{} did not contain a valid base64-encoded 32-byte Ed25519 key.",
                key_path.display()
            );
            std::process::exit(1);
        });

    let issuer_ref = args
        .issuer_ref
        .clone()
        .unwrap_or_else(|| format!("game:{}", args.integrator));

    let client = avalon_sdk::AvalonClient::new(avalon_sdk::AvalonConfig {
        server_url: base,
        integrator_credential_key_id: String::new(),
        integrator_slug: Some(args.integrator.clone()),
        signing_key: Some(key_bytes),
        retry: Default::default(),
    });

    match client.register_issuer(&issuer_ref, target).await {
        Ok(registration) => {
            println!();
            println!(
                "Issuer '{}' registered on {}.",
                registration.issuer_ref, registration.network_id
            );
            println!("Registered at: {}", registration.registered_at);
        }
        Err(e) => {
            eprintln!("register_issuer failed: {e}");
            std::process::exit(1);
        }
    }
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
    fn register_integrator_args_parses_required_fields() {
        let parsed = RegisterIntegratorArgs::parse(&args(&[
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
        assert_eq!(parsed.owner_name, "Ashen Studios");
        assert!(parsed.capabilities.is_empty());
        assert_eq!(parsed.server, None);
    }

    #[test]
    fn register_integrator_args_collects_repeated_capability_flags() {
        let parsed = RegisterIntegratorArgs::parse(&args(&[
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
    fn register_integrator_args_rejects_missing_required_args() {
        let err = RegisterIntegratorArgs::parse(&args(&["--slug", "ashen-realms"]))
            .expect_err("missing --name and --owner-name should fail to parse");
        assert!(err.contains("--name"));
    }

    #[test]
    fn register_integrator_args_rejects_flag_missing_its_value() {
        let err = RegisterIntegratorArgs::parse(&args(&["--slug"]))
            .expect_err("a trailing flag with no value should fail to parse");
        assert!(err.contains("--slug"));
    }

    /// `--developer` is the original spelling of what is now `--owner-name`
    /// (#290), kept working as an alias so existing scripts don't break. The
    /// flag spelling never affects anything downstream, so both produce an
    /// identical `RegisterIntegratorArgs`.
    #[test]
    fn owner_name_flag_accepts_the_deprecated_developer_spelling() {
        let base = ["--slug", "ashen-realms", "--name", "Ashen Realms"];

        let mut with_owner = base.to_vec();
        with_owner.extend_from_slice(&["--owner-name", "Ashen Studios"]);
        let mut with_developer = base.to_vec();
        with_developer.extend_from_slice(&["--developer", "Ashen Studios"]);

        let via_owner_name =
            RegisterIntegratorArgs::parse(&args(&with_owner)).expect("should parse");
        let via_developer =
            RegisterIntegratorArgs::parse(&args(&with_developer)).expect("should parse");

        assert_eq!(via_owner_name.slug, via_developer.slug);
        assert_eq!(via_owner_name.name, via_developer.name);
        assert_eq!(via_owner_name.owner_name, via_developer.owner_name);
        assert_eq!(via_owner_name.owner_name, "Ashen Studios");
    }

    #[test]
    fn register_integrator_args_rejects_unrecognized_flags() {
        let err = RegisterIntegratorArgs::parse(&args(&[
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

    #[test]
    fn issue_achievement_args_parses_required_fields() {
        let parsed = IssueAchievementArgs::parse(&args(&[
            "--integrator",
            "ashen-realms",
            "--achievement",
            "dragon_slayer",
            "--token",
            "test-token",
        ]))
        .expect("should parse with only required args");

        assert_eq!(parsed.integrator, "ashen-realms");
        assert_eq!(parsed.achievement, "dragon_slayer");
        assert_eq!(parsed.token, "test-token");
        assert_eq!(parsed.key_path, None);
        assert_eq!(parsed.key_id, None);
        assert_eq!(parsed.server, None);
    }

    #[test]
    fn issue_achievement_args_accepts_key_and_key_id_and_server_overrides() {
        let parsed = IssueAchievementArgs::parse(&args(&[
            "--integrator",
            "ashen-realms",
            "--achievement",
            "dragon_slayer",
            "--token",
            "test-token",
            "--key",
            "/tmp/my.key",
            "--key-id",
            "11111111-1111-1111-1111-111111111111",
            "--server",
            "http://example.test",
        ]))
        .expect("should parse with all overrides");

        assert_eq!(parsed.key_path.as_deref(), Some("/tmp/my.key"));
        assert_eq!(
            parsed.key_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(parsed.server.as_deref(), Some("http://example.test"));
    }

    #[test]
    fn issue_achievement_args_rejects_missing_required_args() {
        let err = IssueAchievementArgs::parse(&args(&["--integrator", "ashen-realms"]))
            .expect_err("missing --achievement and --token should fail to parse");
        assert!(err.contains("--achievement"));
    }

    #[test]
    fn issue_achievement_args_rejects_unrecognized_flags() {
        let err = IssueAchievementArgs::parse(&args(&[
            "--integrator",
            "ashen-realms",
            "--achievement",
            "dragon_slayer",
            "--token",
            "test-token",
            "--bogus",
            "value",
        ]))
        .expect_err("an unrecognized flag should fail to parse");
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn register_issuer_args_parses_a_literal_network_id() {
        let parsed = RegisterIssuerArgs::parse(&args(&[
            "--integrator",
            "ashen-realms",
            "--network-id",
            "avalon-dev-local",
        ]))
        .expect("should parse with --network-id");
        assert!(matches!(
            parsed.target_network().unwrap(),
            avalon_sdk::network::TargetNetwork::NetworkId(id) if id == "avalon-dev-local"
        ));
    }

    #[test]
    fn register_issuer_args_parses_an_env_shorthand() {
        let parsed =
            RegisterIssuerArgs::parse(&args(&["--integrator", "ashen-realms", "--env", "int"]))
                .expect("should parse with --env");
        assert!(matches!(
            parsed.target_network().unwrap(),
            avalon_sdk::network::TargetNetwork::Env(avalon_sdk::network::TargetNetworkTier::Int)
        ));
    }

    #[test]
    fn register_issuer_args_rejects_neither_network_id_nor_env() {
        let err = RegisterIssuerArgs::parse(&args(&["--integrator", "ashen-realms"]))
            .expect_err("neither --network-id nor --env should fail to parse");
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn register_issuer_args_rejects_both_network_id_and_env() {
        let err = RegisterIssuerArgs::parse(&args(&[
            "--integrator",
            "ashen-realms",
            "--network-id",
            "avalon-dev-local",
            "--env",
            "dev",
        ]))
        .expect_err("both --network-id and --env should fail to parse");
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn register_issuer_args_rejects_an_unrecognized_env() {
        let parsed =
            RegisterIssuerArgs::parse(&args(&["--integrator", "ashen-realms", "--env", "staging"]))
                .expect("parsing itself succeeds — the tier value is only validated later");
        let err = parsed
            .target_network()
            .expect_err("an unrecognized --env value should be rejected");
        assert!(err.contains("staging"));
    }
}
