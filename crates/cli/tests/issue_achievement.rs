//! Black-box test of `avalon issue-achievement` (issue #48) — spawns the
//! real built binary (`env!("CARGO_BIN_EXE_avalon")`) against a real,
//! running `avalon-server` and Postgres, exactly the way an operator would
//! run it from a shell. Gated `--ignored` since it needs live infra — see
//! `make test-live` / `make start`.
//!
//! Setup (identity, integrator registration, achievement definition,
//! consent) goes straight over HTTP rather than through other `avalon`
//! subcommands — `dev_tools` isn't a library this test can import (`cli`
//! has no `lib.rs`, only `main.rs`), and driving each step as its own
//! subprocess would make failures harder to attribute. Only the command
//! actually under test, `issue-achievement`, runs as a real subprocess.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use serde_json::json;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn webauthn_origin() -> String {
    std::env::var("AVALON_WEBAUTHN_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".to_string())
}

fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
    format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
}

/// #697/#698: `signing_key`/`signing_key_id` let a caller sign a later
/// signature-required action (e.g. `POST /integrations/{slug}/connect`)
/// with the same key `register_finish` just registered as this identity's
/// first `identity_signing_keys` row.
struct RegisteredIdentity {
    token: String,
    signing_key: SigningKey,
    signing_key_id: String,
}

async fn register_and_login(
    http: &reqwest::Client,
    base: &str,
    display_name: &str,
) -> RegisteredIdentity {
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
        .unwrap();
    let ticket_id = start["ticket_id"].as_str().unwrap().to_string();
    let creation_options: CredentialCreationOptions =
        serde_json::from_value(start["challenge"].clone()).unwrap();
    let webauthn_credential = client
        .register(
            Origin::from(&origin_url),
            creation_options,
            DefaultClientData,
        )
        .await
        .unwrap();

    let signature = signing_key.sign(&identity_created_signing_bytes(identity_id, display_name));
    let finish = http
        .post(format!("{base}/identities/register/finish"))
        .json(&json!({
            "ticket_id": ticket_id,
            "webauthn_credential": webauthn_credential,
            "event_signing_public_key": event_signing_public_key,
            "event_signature": BASE64.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(finish.status().is_success());

    let session_start: serde_json::Value = http
        .post(format!("{base}/sessions/start"))
        .json(&json!({ "identity_id": identity_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_ticket_id = session_start["ticket_id"].as_str().unwrap().to_string();
    let request_options: CredentialRequestOptions =
        serde_json::from_value(session_start["challenge"].clone()).unwrap();
    let assertion = client
        .authenticate(
            Origin::from(&origin_url),
            request_options,
            DefaultClientData,
        )
        .await
        .unwrap();

    let session_finish = http
        .post(format!("{base}/sessions/finish"))
        .json(&json!({ "ticket_id": session_ticket_id, "credential": assertion }))
        .send()
        .await
        .unwrap();
    assert!(session_finish.status().is_success());
    let login_body: serde_json::Value = session_finish.json().await.unwrap();
    let token = login_body["token"].as_str().unwrap().to_string();

    // register_finish's own event_signing_public_key becomes this
    // identity's first identity_signing_keys row — find its server-
    // assigned id the same way the Hub does (GET /me/devices, match on
    // public key) so a later signature-required call can name it.
    let devices: serde_json::Value = http
        .get(format!("{base}/me/devices"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let signing_key_id = devices
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["public_key"].as_str() == Some(event_signing_public_key.as_str()))
        .expect("register_finish's signing key should be listed")["id"]
        .as_str()
        .unwrap()
        .to_string();

    RegisteredIdentity {
        token,
        signing_key,
        signing_key_id,
    }
}

struct RegisteredIntegrator {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_integrator(http: &reqwest::Client, base: &str) -> RegisteredIntegrator {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("cli-issue-{}", &suffix[..10]);
    let body = json!({
        "slug": slug,
        "name": format!("CLI Issue Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": ["achievements.issue"],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });
    let response = http
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();
    RegisteredIntegrator {
        signing_key,
        slug,
        key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
    }
}

async fn define_achievement(
    http: &reqwest::Client,
    base: &str,
    integrator: &RegisteredIntegrator,
    key: &str,
) {
    let challenge: serde_json::Value = http
        .post(format!("{base}/integrations/{}/challenge", integrator.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = integrator.signing_key.sign(&nonce);

    let response = http
        .post(format!(
            "{base}/integrations/{}/achievements",
            integrator.slug
        ))
        .header("x-avalon-integrator-key-id", &integrator.key_id)
        .header("x-avalon-integrator-challenge-id", challenge_id)
        .header(
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .json(&json!({
            "key": key,
            "name": "Dragon Slayer",
            "description": "Slew the dragon",
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

#[tokio::test]
#[ignore]
async fn issue_achievement_via_the_real_binary_lands_on_the_ledger() {
    let http = reqwest::Client::new();
    let base = server_url();
    let display_name = format!("cli-issue-{}", Uuid::new_v4());

    let identity = register_and_login(&http, &base, &display_name).await;
    let token = identity.token.clone();
    let integrator = register_integrator(&http, &base).await;
    define_achievement(&http, &base, &integrator, "dragon_slayer").await;

    // #697/#698: POST /integrations/{slug}/connect is signature-required.
    let capabilities = vec!["achievements.issue".to_string()];
    let connect_message = format!(
        "avalon:integration.connect:v1:{}:{}",
        integrator.slug,
        capabilities.join(",")
    );
    let connect_signature = identity.signing_key.sign(connect_message.as_bytes());
    let connect = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&json!({
            "capabilities": capabilities,
            "signing_key_id": identity.signing_key_id,
            "signature": BASE64.encode(connect_signature.to_bytes()),
        }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    // Write the signing key to a temp file, exactly the shape
    // `register_integrator` (the real command) saves — this test drives
    // the key/key-id explicitly via flags rather than depending on
    // `dev_tools`'s sidecar file, since this integrator was registered
    // over raw HTTP, not through `avalon register-integrator` itself.
    let key_path = std::env::temp_dir().join(format!("{}.signing-key", integrator.slug));
    std::fs::write(&key_path, BASE64.encode(integrator.signing_key.to_bytes())).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_avalon"))
        .args([
            "issue-achievement",
            "--integrator",
            &integrator.slug,
            "--achievement",
            "dragon_slayer",
            "--token",
            &token,
            "--key",
            key_path.to_str().unwrap(),
            "--key-id",
            &integrator.key_id,
            "--server",
            &base,
        ])
        .output()
        .expect("failed to spawn avalon issue-achievement");

    let _ = std::fs::remove_file(&key_path);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "avalon issue-achievement failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Attestation id:"),
        "unexpected stdout: {stdout}"
    );

    // Confirm it actually landed, not just that the command printed
    // something plausible.
    let history: serde_json::Value = http
        .get(format!("{base}/me/achievements?limit=200"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let achievements = history["achievements"].as_array().unwrap();
    assert_eq!(achievements.len(), 1);
    assert_eq!(
        achievements[0]["achievement"],
        format!("game:{}:achievement:dragon_slayer", integrator.slug)
    );
}
