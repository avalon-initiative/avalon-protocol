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
    let upper = someone_elses_id.to_uppercase();
    let mixed: String = someone_elses_id
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    for name in [
        someone_elses_id.clone(),
        upper,
        mixed,
        format!("  {someone_elses_id}\t"),
        format!("\u{200B}{someone_elses_id}"),
        format!("{someone_elses_id}\u{200D}\u{2060}"),
    ] {
        let (status, body) = start(json!({
            "identity_id": id,
            "event_signing_public_key": BASE64.encode(public_key),
            "display_name": name,
        }))
        .await;
        assert_eq!(status, 400, "{name:?}");
        assert_eq!(body["code"], "INVALID_DISPLAY_NAME");
    }
}

#[tokio::test]
#[ignore]
async fn identity_path_parameters_in_any_other_shape_are_a_bad_request() {
    let http = reqwest::Client::new();
    for bad in [
        "00000000-0000-0000-0000-000000000000",
        "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789",
        "short",
    ] {
        let response = http
            .get(format!("{}/identities/{bad}/locations", server_url()))
            .send()
            .await
            .expect("request failed — is `make start` running?");
        assert_eq!(response.status().as_u16(), 400, "{bad}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["code"], "INVALID_IDENTITY_ID", "{bad}");
    }

    // A valid identity id with a malformed non-identity part is a different error code.
    let (_, id) = fresh_key();
    let response = http
        .delete(format!("{}/guilds/not-a-uuid/members/{id}", server_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "INVALID_PATH_PARAMETER");
}

#[tokio::test]
#[ignore]
async fn unacceptable_keys_and_encodings_are_rejected() {
    let (public_key, id) = fresh_key();
    // y = 2^255 - 19 + 1: a non-canonical encoding of y = 1 (small order as well).
    let mut non_canonical = [0xffu8; 32];
    non_canonical[0] = 0xee;
    non_canonical[31] = 0x7f;
    let non_canonical_id =
        avalon_protocol::identity_id::derive_identity_id(&non_canonical).to_string();
    let url_safe = base64::engine::general_purpose::URL_SAFE.encode([0xfbu8; 32]);
    let cases: Vec<(String, String)> = vec![
        (id.clone(), BASE64.encode(&public_key[..31])),
        (
            id.clone(),
            BASE64.encode([public_key.as_slice(), &[0u8]].concat()),
        ),
        (
            id.clone(),
            BASE64.encode(public_key).trim_end_matches('=').to_string(),
        ),
        (id.clone(), format!(" {}", BASE64.encode(public_key))),
        (id.clone(), hex::encode(public_key)),
        (id.clone(), url_safe),
        (id.clone(), String::new()),
        (non_canonical_id, BASE64.encode(non_canonical)),
    ];
    for (identity_id, key) in cases {
        let (status, body) = start(json!({
            "identity_id": identity_id,
            "event_signing_public_key": key,
            "display_name": "id-bad-key",
        }))
        .await;
        assert_eq!(status, 400, "{key:?}: {body}");
        assert_eq!(body["code"], "INVALID_IDENTITY_ID", "{key:?}");
    }
}
