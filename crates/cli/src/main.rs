//! `avalon` — local dev/ops CLI.
//!
//! Commands: `avalon inspect-ledger`, `avalon inspect-ledger-full` (same
//! view, plus each entry's payload), `avalon create-identity`,
//! `avalon login <identity_id>` (issue #115 — drives a real login ceremony
//! against a passkey `create-identity` saved locally, prints a session
//! token), `avalon outbox-status`. `register-game` and `issue-achievement`
//! (per `docs/Proposal.md` §23's milestone-1 vertical slice) aren't wired up
//! yet — they depend on the Game Registration and Achievements epics, still
//! unbuilt.

use std::io::Write as _;
use std::path::PathBuf;

use avalon_chain::PostgresSettlementProvider;
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
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let mut args = std::env::args();
    let command = args.nth(1);
    match command.as_deref() {
        Some("inspect-ledger") => inspect_ledger(false).await,
        Some("inspect-ledger-full") => inspect_ledger(true).await,
        Some("create-identity") => create_identity().await,
        Some("login") => {
            let Some(identity_id) = args.next().and_then(|s| s.parse::<Uuid>().ok()) else {
                eprintln!("usage: avalon login <identity_id>");
                std::process::exit(1);
            };
            login(identity_id).await;
        }
        Some("outbox-status") => outbox_status().await,
        _ => {
            eprintln!(
                "usage: avalon <inspect-ledger|inspect-ledger-full|create-identity|login <identity_id>|outbox-status>"
            );
            std::process::exit(1);
        }
    }
}

fn server_url() -> String {
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

async fn create_identity() {
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
    let mut csprng = rand::rngs::OsRng;
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
async fn login(identity_id: Uuid) {
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

async fn outbox_status() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    let status = avalon_server::outbox::status(&pool)
        .await
        .expect("failed to read outbox status");

    match status.oldest_pending {
        None => println!("outbox: empty, nothing pending"),
        Some(oldest) => println!(
            "outbox: {} pending, oldest enqueued at {oldest}",
            status.pending_count
        ),
    }
}

/// `full: false` is `avalon inspect-ledger` — the concise chain-integrity
/// view. `full: true` is `avalon inspect-ledger-full` — the same view plus
/// each entry's actual payload (pretty-printed JSON) and version, for
/// answering "what's actually inside this block" rather than just "is the
/// chain intact." Same query either way (`list_entries` always fetches the
/// payload, since it needs it to re-verify each entry's hash) — this only
/// changes what gets printed.
async fn inspect_ledger(full: bool) {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    let chain = PostgresSettlementProvider::new(pool);
    let entries = chain.list_entries().await.expect("failed to read ledger");

    if entries.is_empty() {
        println!("(ledger is empty)");
        return;
    }

    for entry in &entries {
        let verified = if entry.chain_intact {
            "✓"
        } else {
            "✗ BROKEN CHAIN"
        };
        println!(
            "┌─ Block #{} ─────────────────────────────────────────",
            entry.seq
        );
        println!("│ kind:      {}", entry.kind);
        println!("│ issuer:    {}", entry.issuer);
        println!("│ subject:   {}", entry.subject);
        println!("│ timestamp: {}", entry.event_timestamp);
        if full {
            println!("│ version:   {}", entry.version);
            let pretty = serde_json::to_string_pretty(&entry.payload)
                .unwrap_or_else(|_| entry.payload.to_string());
            println!("│ payload:");
            for line in pretty.lines() {
                println!("│   {line}");
            }
        }
        println!("│ hash:      {}", short_hash(&entry.entry_hash));
        println!("│ prev:      {}", short_hash(&entry.prev_hash));
        println!("│ verified:  {verified}");
        println!("└──────────────────────────────────────────────────────");
    }

    let broken = entries.iter().filter(|e| !e.chain_intact).count();
    println!();
    println!(
        "{} entries, {}",
        entries.len(),
        if broken == 0 {
            "chain intact ✓".to_string()
        } else {
            format!("{broken} broken link(s) ✗")
        }
    );
}

fn short_hash(hash: &str) -> String {
    if hash.len() <= 16 {
        hash.to_string()
    } else {
        format!("{}...{}", &hash[..8], &hash[hash.len() - 8..])
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
}
