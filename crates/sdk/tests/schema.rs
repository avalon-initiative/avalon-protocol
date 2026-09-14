//! Exercises `#[derive(AvalonSchema)]` and `Session::publish_schema_version`/
//! `publish_instance` (issue #386) against a real, running `avalon-server`
//! and Postgres. Gated `--ignored` since it needs live infra — see
//! `make test-live` / `make start`.
//!
//! Setup mirrors `achievements.rs`'s own live test: a real WebAuthn
//! ceremony creates the user identity, a real integrator registers and the
//! user connects to it (establishing the active binding
//! `integrator_data::publish_instance` requires), then the SDK — never the
//! raw HTTP API — publishes a schema derived from an ordinary Rust struct
//! and an instance of it, and reads the instance back via
//! `GET /identities/{id}/integrator-data` to prove the full round-trip,
//! including that the derived (snake_case) proto field names actually
//! parse against `serde_json::to_value` of the same struct with no
//! `#[serde(rename_all = ...)]` needed.

use avalon_sdk::schema::AvalonSchema;
use avalon_sdk::{AvalonClient, AvalonConfig};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn webauthn_origin() -> String {
    std::env::var("AVALON_WEBAUTHN_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".to_string())
}

/// Must match `avalon-server`'s `handlers::identity_created_signing_bytes`
/// exactly — duplicated here since this test doesn't depend on
/// `avalon-server`, matching `achievements.rs`'s own copy.
fn identity_created_signing_bytes(identity_id: Uuid, display_name: &str) -> Vec<u8> {
    format!("avalon:identity.created:v1:{identity_id}:{display_name}").into_bytes()
}

async fn register_and_login(
    http: &reqwest::Client,
    base: &str,
    display_name: &str,
) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    let origin_str = webauthn_origin();
    let origin_url =
        url::Url::parse(&origin_str).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL");

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
        .expect("register/start request failed — is `make start` running?")
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
        serde_json::from_value(session_start["challenge"].clone()).expect("bad request challenge");

    let assertion = client
        .authenticate(
            Origin::from(&origin_url),
            request_options,
            DefaultClientData,
        )
        .await
        .expect("WebAuthn authentication ceremony failed");

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
    (identity_id, token)
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
    let slug = format!("sdk-schema-{}", &suffix[..10]);
    let body = json!({
        "slug": slug,
        "name": format!("SDK Schema Test {}", &suffix[..8]),
        "owner_name": "Test Studio",
        "requested_capabilities": [],
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

/// The struct under test — exercises a scalar, an optional field, and a
/// repeated field, plus one explicit field-visibility override, all in one
/// derive.
#[derive(Debug, Serialize, AvalonSchema)]
struct CharacterProgress {
    level: u32,
    xp: u64,
    #[avalon(visibility = "private")]
    guild_tag: Option<String>,
    completed_quests: Vec<String>,
}

#[tokio::test]
#[ignore]
async fn derives_and_publishes_a_schema_then_an_instance_and_reads_it_back() {
    let http = reqwest::Client::new();
    let base = server_url();
    let display_name = format!("sdk-schema-{}", Uuid::new_v4());

    let (identity_id, token) = register_and_login(&http, &base, &display_name).await;
    let integrator = register_integrator(&http, &base).await;

    // The user's own consent: an active binding. This ticket's own write
    // paths (`authenticate_owning_integrator`) don't check any specific
    // `permission_grants` capability — only `has_active_binding` — so the
    // connect call carries no capabilities.
    let connect = http
        .post(format!("{base}/integrations/{}/connect", integrator.slug))
        .bearer_auth(&token)
        .json(&json!({ "capabilities": [] }))
        .send()
        .await
        .unwrap();
    assert!(connect.status().is_success(), "{:?}", connect.status());

    let client = AvalonClient::new(AvalonConfig {
        server_url: base.clone(),
        integrator_credential_key_id: integrator.key_id.clone(),
        integrator_slug: Some(integrator.slug.clone()),
        signing_key: Some(integrator.signing_key.to_bytes()),
        retry: Default::default(),
    });
    let session = client
        .authenticate(&token)
        .await
        .expect("authenticate() should succeed with a valid session token");

    let expected_proto = "syntax = \"proto3\";\n\nmessage CharacterProgress {\n  uint32 level = 1;\n  uint64 xp = 2;\n  optional string guild_tag = 3;\n  repeated string completed_quests = 4;\n}\n";
    assert_eq!(CharacterProgress::proto_source(), expected_proto);

    let published_schema = session
        .publish_schema_version::<CharacterProgress>()
        .await
        .expect("publish_schema_version should succeed for a freshly connected integrator");
    assert_eq!(published_schema.version, 1);
    assert_eq!(published_schema.default_visibility, "public");
    assert_eq!(
        published_schema
            .field_visibility
            .get("guild_tag")
            .map(String::as_str),
        Some("private")
    );

    let instance = CharacterProgress {
        level: 42,
        xp: 133_700,
        guild_tag: Some("AVLN".to_string()),
        completed_quests: vec!["dragon_slayer".to_string(), "lost_city".to_string()],
    };
    let published_instance = session
        .publish_instance(published_schema.version, &instance)
        .await
        .expect(
            "publish_instance should succeed once the schema is published and the subject is bound",
        );
    assert_eq!(published_instance.subject, identity_id);

    // Read it back through the public, unauthenticated read surface —
    // proves the instance actually round-tripped through the server's own
    // protobuf-json validation (#384), not just that our POST returned 2xx.
    let read_back: serde_json::Value = http
        .get(format!("{base}/identities/{identity_id}/integrator-data"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let instances = read_back
        .as_array()
        .expect("GET /identities/{id}/integrator-data should return a JSON array");
    let found = instances
        .iter()
        .find(|entry| entry["schema"] == published_instance.schema_id)
        .expect("published instance should be visible in the public read-back");
    assert_eq!(found["fields"]["level"], 42);
    assert_eq!(found["fields"]["xp"], 133_700);
    assert_eq!(
        found["fields"]["completed_quests"],
        json!(["dragon_slayer", "lost_city"])
    );
    // `guild_tag` was published as `visibility = "private"` and this read
    // is unauthenticated (no relationship to the publishing integrator) —
    // it must be redacted from `fields` entirely, never leaked as plain
    // JSON (`resolve_visible_fields` omits the key, it doesn't null it).
    assert!(
        found["fields"].get("guild_tag").is_none(),
        "private field guild_tag should not be visible to an unauthenticated reader: {:?}",
        found["fields"]
    );
}
