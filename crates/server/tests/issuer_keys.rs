//! Exercises the issuer key-lifecycle endpoints (issue #84, implementing
//! #80's decided two-tier root/operational key model) against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

struct RegisteredGame {
    slug: String,
    game_id: String,
    root_key_id: String,
    root_signing_key: SigningKey,
}

async fn register_game(http: &reqwest::Client) -> RegisteredGame {
    let base = server_url();
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let root_signing_key = SigningKey::generate(&mut csprng);
    let body = serde_json::json!({
        "slug": format!("test-issuer-keys-{}", &suffix[..12]),
        "name": format!("Issuer Key Test {}", &suffix[..8]),
        "developer": "Test Studio",
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(root_signing_key.verifying_key().as_bytes()),
        },
    });
    let response = http
        .post(format!("{base}/games"))
        .json(&body)
        .send()
        .await
        .expect("register game failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: Value = response.json().await.unwrap();

    RegisteredGame {
        slug: registered["slug"].as_str().unwrap().to_string(),
        game_id: registered["id"].as_str().unwrap().to_string(),
        root_key_id: registered["credential"]["key_id"]
            .as_str()
            .unwrap()
            .to_string(),
        root_signing_key,
    }
}

/// Runs one challenge-response round: fetches a fresh challenge for `slug`,
/// signs its nonce with `signing_key`, and returns the three headers the
/// signature-carrying request needs — same shape
/// `the_challenge_response_auth_flow_round_trips` in `tests/games.rs`
/// already establishes for ordinary (non-root) auth.
async fn signed_challenge_headers(
    http: &reqwest::Client,
    slug: &str,
    key_id: &str,
    signing_key: &SigningKey,
) -> [(&'static str, String); 3] {
    let base = server_url();
    let challenge = http
        .post(format!("{base}/games/{slug}/challenge"))
        .send()
        .await
        .unwrap();
    assert!(challenge.status().is_success(), "{:?}", challenge.status());
    let challenge: Value = challenge.json().await.unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap().to_string();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = signing_key.sign(&nonce);

    [
        ("x-avalon-integrator-key-id", key_id.to_string()),
        ("x-avalon-integrator-challenge-id", challenge_id),
        (
            "x-avalon-integrator-signature",
            BASE64.encode(signature.to_bytes()),
        ),
    ]
}

fn with_headers(
    builder: reqwest::RequestBuilder,
    headers: &[(&'static str, String)],
) -> reqwest::RequestBuilder {
    headers
        .iter()
        .fold(builder, |b, (name, value)| b.header(*name, value))
}

#[tokio::test]
#[ignore]
async fn root_key_can_add_an_operational_key_and_it_authenticates_ordinary_calls() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = register_game(&http).await;

    let operational_key = SigningKey::generate(&mut rand::rng());
    let headers =
        signed_challenge_headers(&http, &game.slug, &game.root_key_id, &game.root_signing_key)
            .await;
    let add = with_headers(
        http.post(format!("{base}/games/{}/keys", game.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(operational_key.verifying_key().as_bytes()),
        "role": "operational",
    }))
    .send()
    .await
    .unwrap();
    assert!(add.status().is_success(), "{:?}", add.status());
    let added: Value = add.json().await.unwrap();
    assert_eq!(added["role"].as_str().unwrap(), "operational");
    let op_key_id = added["key_id"].as_str().unwrap().to_string();

    // The freshly-added operational key authenticates an ordinary call.
    let headers = signed_challenge_headers(&http, &game.slug, &op_key_id, &operational_key).await;
    let whoami = with_headers(http.get(format!("{base}/games/whoami")), &headers)
        .send()
        .await
        .unwrap();
    assert!(whoami.status().is_success(), "{:?}", whoami.status());
    let whoami: Value = whoami.json().await.unwrap();
    assert_eq!(whoami["game_id"].as_str().unwrap(), game.game_id);
}

#[tokio::test]
#[ignore]
async fn an_operational_key_cannot_authorize_a_key_set_change() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = register_game(&http).await;

    let operational_key = SigningKey::generate(&mut rand::rng());
    let headers =
        signed_challenge_headers(&http, &game.slug, &game.root_key_id, &game.root_signing_key)
            .await;
    let add = with_headers(
        http.post(format!("{base}/games/{}/keys", game.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(operational_key.verifying_key().as_bytes()),
        "role": "operational",
    }))
    .send()
    .await
    .unwrap();
    assert!(add.status().is_success());
    let op_key_id = add.json::<Value>().await.unwrap()["key_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The operational key tries to add another key — must be rejected.
    let another_key = SigningKey::generate(&mut rand::rng());
    let headers = signed_challenge_headers(&http, &game.slug, &op_key_id, &operational_key).await;
    let attempt = with_headers(
        http.post(format!("{base}/games/{}/keys", game.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(another_key.verifying_key().as_bytes()),
        "role": "operational",
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(attempt.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn root_revokes_an_operational_key_and_it_can_no_longer_authenticate_anything() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = register_game(&http).await;

    let operational_key = SigningKey::generate(&mut rand::rng());
    let headers =
        signed_challenge_headers(&http, &game.slug, &game.root_key_id, &game.root_signing_key)
            .await;
    let add = with_headers(
        http.post(format!("{base}/games/{}/keys", game.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(operational_key.verifying_key().as_bytes()),
        "role": "operational",
    }))
    .send()
    .await
    .unwrap();
    let op_key_id = add.json::<Value>().await.unwrap()["key_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Root revokes it.
    let headers =
        signed_challenge_headers(&http, &game.slug, &game.root_key_id, &game.root_signing_key)
            .await;
    let revoke = with_headers(
        http.post(format!(
            "{base}/games/{}/keys/{}/revoke",
            game.slug, op_key_id
        )),
        &headers,
    )
    .json(&serde_json::json!({ "reason": "test" }))
    .send()
    .await
    .unwrap();
    assert!(revoke.status().is_success(), "{:?}", revoke.status());
    let revoked: Value = revoke.json().await.unwrap();
    assert!(revoked["revoked_at"].is_string());

    // The revoked key can no longer authenticate anything.
    let headers = signed_challenge_headers(&http, &game.slug, &op_key_id, &operational_key).await;
    let whoami = with_headers(http.get(format!("{base}/games/whoami")), &headers)
        .send()
        .await
        .unwrap();
    assert_eq!(whoami.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Revoking it again is rejected, not a silent no-op.
    let headers =
        signed_challenge_headers(&http, &game.slug, &game.root_key_id, &game.root_signing_key)
            .await;
    let re_revoke = with_headers(
        http.post(format!(
            "{base}/games/{}/keys/{}/revoke",
            game.slug, op_key_id
        )),
        &headers,
    )
    .json(&serde_json::json!({ "reason": "retry" }))
    .send()
    .await
    .unwrap();
    assert_eq!(re_revoke.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn a_games_root_key_cannot_manage_a_different_games_keys() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game_a = register_game(&http).await;
    let game_b = register_game(&http).await;

    let intruding_key = SigningKey::generate(&mut rand::rng());
    // Authenticate as game A's root key, but target game B's slug.
    let headers = signed_challenge_headers(
        &http,
        &game_a.slug,
        &game_a.root_key_id,
        &game_a.root_signing_key,
    )
    .await;
    let attempt = with_headers(
        http.post(format!("{base}/games/{}/keys", game_b.slug)),
        &headers,
    )
    .json(&serde_json::json!({
        "algorithm": "ed25519",
        "public_key": BASE64.encode(intruding_key.verifying_key().as_bytes()),
        "role": "operational",
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(attempt.status(), reqwest::StatusCode::FORBIDDEN);
}
