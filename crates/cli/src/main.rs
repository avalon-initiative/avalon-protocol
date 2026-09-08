//! `avalon` — local dev/ops CLI.
//!
//! Commands: `avalon inspect-ledger`, `avalon create-identity`,
//! `avalon outbox-status`. `register-game` and `issue-achievement` (per
//! `docs/Proposal.md` §23's milestone-1 vertical slice) aren't wired up yet —
//! they depend on the Game Registration and Achievements epics, still
//! unbuilt.

use std::io::Write as _;
use std::path::PathBuf;

use avalon_chain::PostgresSettlementProvider;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("inspect-ledger") => inspect_ledger().await,
        Some("create-identity") => create_identity().await,
        Some("outbox-status") => outbox_status().await,
        _ => {
            eprintln!("usage: avalon <inspect-ledger|create-identity|outbox-status>");
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

    // The passkey lives with whatever authenticator holds it (a real device,
    // once the Hub exists) — this CLI never persists it. The event-signing
    // key is different: nothing in this milestone lets a player carry it
    // between sessions except a local file, so it's saved here for later
    // reuse (e.g. a future `profile.updated`-signing command, issue #86).
    let key_dir = PathBuf::from("_running/keys");
    std::fs::create_dir_all(&key_dir).ok();
    let key_path = key_dir.join(format!("{identity_id}.signing-key"));
    std::fs::write(&key_path, BASE64.encode(signing_key.to_bytes()))
        .expect("failed to write signing key file");

    println!();
    println!("Identity created: {identity_id}");
    println!("Event-signing key saved to: {}", key_path.display());
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

async fn inspect_ledger() {
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
