//! A registration signature is bound to its ceremony: a signature copied from a public ledger
//! entry (or from another ticket) must not complete a registration. Gated `--ignored`; needs a
//! running server (`make start`).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
use passkey_client::{Client, DefaultClientData, Origin};
use passkey_types::ctap2::Aaguid;
use passkey_types::webauthn::CredentialCreationOptions;
use serde_json::{json, Value};
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn rp_origin() -> url::Url {
    let origin = std::env::var("AVALON_WEBAUTHN_ORIGIN")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    url::Url::parse(&origin).expect("AVALON_WEBAUTHN_ORIGIN must be a valid URL")
}

struct Started {
    ticket_id: Uuid,
    network_id: String,
    shard_id: String,
    challenge: Value,
}

async fn start(http: &reqwest::Client, key: &SigningKey, display_name: &str) -> Started {
    let public_key = key.verifying_key().to_bytes();
    let id = avalon_protocol::identity_id::derive_identity_id(&public_key);
    let response: Value = http
        .post(format!("{}/identities/register/start", server_url()))
        .json(&json!({
            "identity_id": id,
            "event_signing_public_key": BASE64.encode(public_key),
            "display_name": display_name,
        }))
        .send()
        .await
        .expect("register/start failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    Started {
        ticket_id: response["ticket_id"].as_str().unwrap().parse().unwrap(),
        network_id: response["network_id"].as_str().unwrap().to_string(),
        shard_id: response["shard_id"].as_str().unwrap().to_string(),
        challenge: response["challenge"].clone(),
    }
}

fn sign(key: &SigningKey, network_id: &str, shard_id: &str, ticket_id: Uuid, name: &str) -> String {
    let public_key = key.verifying_key().to_bytes();
    let id = avalon_protocol::identity_id::derive_identity_id(&public_key);
    let bytes = avalon_protocol::identity_id::identity_created_signing_bytes_v2(
        network_id,
        shard_id,
        ticket_id,
        &id,
        &public_key,
        name,
    );
    BASE64.encode(key.sign(&bytes).to_bytes())
}

async fn finish(
    http: &reqwest::Client,
    started: &Started,
    credential: &Value,
    event_signature: &str,
) -> reqwest::Response {
    http.post(format!("{}/identities/register/finish", server_url()))
        .json(&json!({
            "ticket_id": started.ticket_id,
            "webauthn_credential": credential,
            "event_signature": event_signature,
            "device_label": null,
        }))
        .send()
        .await
        .unwrap()
}

async fn passkey(started: &Started) -> Value {
    let mut client = Client::new(Authenticator::new(
        Aaguid::new_empty(),
        MemoryStore::new(),
        MockUserValidationMethod::verified_user(1),
    ))
    .allows_insecure_localhost(true);
    let options: CredentialCreationOptions =
        serde_json::from_value(started.challenge.clone()).unwrap();
    let credential = client
        .register(Origin::from(&rp_origin()), options, DefaultClientData)
        .await
        .expect("virtual authenticator registration should succeed");
    serde_json::to_value(credential).unwrap()
}

#[tokio::test]
#[ignore]
async fn a_signature_from_another_ticket_does_not_complete_a_registration() {
    let http = reqwest::Client::new();
    let key = SigningKey::generate(&mut rand::rng());
    let id = avalon_protocol::identity_id::derive_identity_id(&key.verifying_key().to_bytes());
    let name = format!("replay-{id}");

    // The victim's own, honestly signed ceremony (its signature is public once it reaches a ledger).
    let victim = start(&http, &key, &name).await;
    let copied = sign(
        &key,
        &victim.network_id,
        &victim.shard_id,
        victim.ticket_id,
        &name,
    );

    // An attacker starts their own ceremony for the same id and key, with their own passkey.
    let attacker = start(&http, &key, &name).await;
    let attacker_passkey = passkey(&attacker).await;
    let replayed = finish(&http, &attacker, &attacker_passkey, &copied).await;
    assert_eq!(
        replayed.status().as_u16(),
        401,
        "a copied signature must be refused"
    );
    let body: Value = replayed.json().await.unwrap();
    assert_eq!(body["code"], "INVALID_EVENT_SIGNATURE");
}

#[tokio::test]
#[ignore]
async fn a_signature_for_another_network_is_refused_and_the_right_one_passes() {
    let http = reqwest::Client::new();
    let key = SigningKey::generate(&mut rand::rng());
    let id = avalon_protocol::identity_id::derive_identity_id(&key.verifying_key().to_bytes());
    let name = format!("net-{id}");

    let wrong_network = start(&http, &key, &name).await;
    let passkey_a = passkey(&wrong_network).await;
    let bad = sign(
        &key,
        "some-other-network",
        &wrong_network.shard_id,
        wrong_network.ticket_id,
        &name,
    );
    let refused = finish(&http, &wrong_network, &passkey_a, &bad).await;
    assert_eq!(refused.status().as_u16(), 401);

    let honest = start(&http, &key, &name).await;
    let passkey_b = passkey(&honest).await;
    let good = sign(
        &key,
        &honest.network_id,
        &honest.shard_id,
        honest.ticket_id,
        &name,
    );
    let accepted = finish(&http, &honest, &passkey_b, &good).await;
    assert!(accepted.status().is_success(), "{:?}", accepted.status());

    // The ticket is single-use.
    let reused = finish(&http, &honest, &passkey_b, &good).await;
    assert!(reused.status().is_client_error());
}

#[tokio::test]
#[ignore]
async fn a_bad_signature_and_a_signature_by_another_key_are_refused() {
    let http = reqwest::Client::new();
    let key = SigningKey::generate(&mut rand::rng());
    let id = avalon_protocol::identity_id::derive_identity_id(&key.verifying_key().to_bytes());
    let name = format!("badsig-{id}");

    let started = start(&http, &key, &name).await;
    let credential = passkey(&started).await;
    let by_other_key = sign(
        &SigningKey::generate(&mut rand::rng()),
        &started.network_id,
        &started.shard_id,
        started.ticket_id,
        &name,
    );
    let refused = finish(&http, &started, &credential, &by_other_key).await;
    assert_eq!(refused.status().as_u16(), 401);

    // The ceremony row is consumed by the failed attempt, so even the right signature now fails.
    let good = sign(
        &key,
        &started.network_id,
        &started.shard_id,
        started.ticket_id,
        &name,
    );
    let after = finish(&http, &started, &credential, &good).await;
    assert!(after.status().is_client_error());

    let garbage = start(&http, &key, &name).await;
    let credential = passkey(&garbage).await;
    let refused = finish(&http, &garbage, &credential, "not base64!!").await;
    assert_eq!(refused.status().as_u16(), 401);
}

#[tokio::test]
#[ignore]
async fn a_signature_for_another_shard_is_refused() {
    let http = reqwest::Client::new();
    let key = SigningKey::generate(&mut rand::rng());
    let id = avalon_protocol::identity_id::derive_identity_id(&key.verifying_key().to_bytes());
    let name = format!("shard-{id}");
    let started = start(&http, &key, &name).await;
    let credential = passkey(&started).await;
    let other_shard = sign(
        &key,
        &started.network_id,
        "game:other/1",
        started.ticket_id,
        &name,
    );
    let refused = finish(&http, &started, &credential, &other_shard).await;
    assert_eq!(refused.status().as_u16(), 401);
}
