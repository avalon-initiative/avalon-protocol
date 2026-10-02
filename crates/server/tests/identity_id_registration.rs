//! `register/start` refuses ids that are not derived from the submitted inception key, weak keys
//! and display names shaped like an identity id. Gated `--ignored` since it needs a running
//! server (`make start`).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

async fn start(body: Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("{}/identities/register/start", server_url()))
        .json(&body)
        .send()
        .await
        .expect("register/start failed — is `make start` running?");
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

fn fresh_key() -> ([u8; 32], String) {
    let key = SigningKey::generate(&mut rand::rng());
    let public_key = key.verifying_key().to_bytes();
    let id = avalon_protocol::identity_id::derive_identity_id(&public_key).to_string();
    (public_key, id)
}

#[tokio::test]
#[ignore]
async fn a_matching_id_and_key_starts_a_ceremony() {
    let (public_key, id) = fresh_key();
    let (status, body) = start(json!({
        "identity_id": id,
        "event_signing_public_key": BASE64.encode(public_key),
        "display_name": format!("id-reg-ok-{id}"),
    }))
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["ticket_id"].is_string());
}

#[tokio::test]
#[ignore]
async fn an_id_not_derived_from_the_key_is_rejected() {
    let (public_key, _) = fresh_key();
    let (_, other_id) = fresh_key();
    let (status, body) = start(json!({
        "identity_id": other_id,
        "event_signing_public_key": BASE64.encode(public_key),
        "display_name": "id-mismatch",
    }))
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "IDENTITY_ID_MISMATCH");
}

#[tokio::test]
#[ignore]
async fn malformed_ids_and_weak_keys_are_rejected() {
    let (public_key, id) = fresh_key();
    for bad_id in [
        id.to_uppercase(),
        id[..63].to_string(),
        "00000000-0000-0000-0000-000000000000".to_string(),
    ] {
        let (status, body) = start(json!({
            "identity_id": bad_id,
            "event_signing_public_key": BASE64.encode(public_key),
            "display_name": "id-malformed",
        }))
        .await;
        assert_eq!(status, 400, "{bad_id}: {body}");
        assert_eq!(body["code"], "INVALID_IDENTITY_ID");
    }

    // The identity point is a small-order key: acceptable bytes, unacceptable key.
    let mut weak = [0u8; 32];
    weak[0] = 1;
    let weak_id = avalon_protocol::identity_id::derive_identity_id(&weak).to_string();
    let (status, body) = start(json!({
        "identity_id": weak_id,
        "event_signing_public_key": BASE64.encode(weak),
        "display_name": "id-weak-key",
    }))
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "INVALID_IDENTITY_ID");
}

#[tokio::test]
#[ignore]
async fn a_display_name_shaped_like_an_identity_id_is_rejected() {
    let (public_key, id) = fresh_key();
    let (_, someone_elses_id) = fresh_key();
    let (status, body) = start(json!({
        "identity_id": id,
        "event_signing_public_key": BASE64.encode(public_key),
        "display_name": someone_elses_id,
    }))
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "INVALID_DISPLAY_NAME");
}
