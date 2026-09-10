//! Exercises game registration and the challenge-response game-auth flow
//! (issue #26) against a real, running `avalon-server` and Postgres. Gated
//! `--ignored` since it needs live infra — see `make test-live` / `make
//! start`. Skipped in this sandbox per `.claude/CLAUDE.md` (no reachable
//! Postgres here); written but not run against a live database.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

/// A fresh Ed25519 keypair plus the registration body it belongs in — a
/// short random slug/name pair so concurrent test runs never collide on the
/// unique `slug` constraint, mirroring `unique_guild_body()` in
/// `crates/server/tests/guilds.rs`.
struct UnregisteredGame {
    signing_key: SigningKey,
    body: serde_json::Value,
}

fn unique_game() -> UnregisteredGame {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let body = serde_json::json!({
        "slug": format!("test-game-{}", &suffix[..12]),
        "name": format!("Test Game {}", &suffix[..8]),
        "developer": "Test Studio",
        "requested_capabilities": ["presence.read", "friends.read"],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });
    UnregisteredGame { signing_key, body }
}

#[tokio::test]
#[ignore]
async fn registering_a_game_returns_the_game_and_its_credential() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = unique_game();

    let response = http
        .post(format!("{base}/games"))
        .json(&game.body)
        .send()
        .await
        .expect("register game failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());

    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["slug"].as_str().unwrap(), game.body["slug"]);
    assert_eq!(body["name"].as_str().unwrap(), game.body["name"]);
    assert_eq!(body["developer"].as_str().unwrap(), game.body["developer"]);
    assert_eq!(body["status"].as_str().unwrap(), "active");
    assert_eq!(body["requested_capabilities"].as_array().unwrap().len(), 2);
    let credential = &body["credential"];
    assert_eq!(
        credential["game_id"].as_str().unwrap(),
        body["id"].as_str().unwrap()
    );
    assert!(!credential["key_id"].as_str().unwrap().is_empty());
}

#[tokio::test]
#[ignore]
async fn a_second_registration_with_the_same_slug_conflicts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = unique_game();

    let first = http
        .post(format!("{base}/games"))
        .json(&game.body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // Different developer/key, same slug — should still collide on slug
    // alone, matching the invariant that a game cannot register with
    // another game's slug.
    let mut second_body = game.body.clone();
    second_body["developer"] = serde_json::json!("A Different Studio");
    let second = http
        .post(format!("{base}/games"))
        .json(&second_body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn an_uppercase_slug_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let mut game = unique_game();
    game.body["slug"] = serde_json::json!("Not-Lowercase");

    let response = http
        .post(format!("{base}/games"))
        .json(&game.body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn the_challenge_response_auth_flow_round_trips() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = unique_game();

    let register = http
        .post(format!("{base}/games"))
        .json(&game.body)
        .send()
        .await
        .unwrap();
    assert!(register.status().is_success());
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let key_id = registered["credential"]["key_id"].as_str().unwrap();

    let challenge = http
        .post(format!("{base}/games/{slug}/challenge"))
        .send()
        .await
        .unwrap();
    assert!(challenge.status().is_success(), "{:?}", challenge.status());
    let challenge: serde_json::Value = challenge.json().await.unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();

    let signature = game.signing_key.sign(&nonce);

    let whoami = http
        .get(format!("{base}/games/whoami"))
        .header("x-avalon-game-key-id", key_id)
        .header("x-avalon-game-challenge-id", challenge_id)
        .header(
            "x-avalon-game-signature",
            BASE64.encode(signature.to_bytes()),
        )
        .send()
        .await
        .unwrap();
    assert!(whoami.status().is_success(), "{:?}", whoami.status());
    let whoami: serde_json::Value = whoami.json().await.unwrap();
    assert_eq!(
        whoami["game_id"].as_str().unwrap(),
        registered["id"].as_str().unwrap()
    );
}

#[tokio::test]
#[ignore]
async fn a_challenge_cannot_be_replayed() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = unique_game();

    let register = http
        .post(format!("{base}/games"))
        .json(&game.body)
        .send()
        .await
        .unwrap();
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let key_id = registered["credential"]["key_id"].as_str().unwrap();

    let challenge: serde_json::Value = http
        .post(format!("{base}/games/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature_b64 = BASE64.encode(game.signing_key.sign(&nonce).to_bytes());

    let first = http
        .get(format!("{base}/games/whoami"))
        .header("x-avalon-game-key-id", key_id)
        .header("x-avalon-game-challenge-id", challenge_id)
        .header("x-avalon-game-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    // The same (challenge_id, signature) pair a second time — the
    // challenge row was already consumed by the first request.
    let second = http
        .get(format!("{base}/games/whoami"))
        .header("x-avalon-game-key-id", key_id)
        .header("x-avalon-game-challenge-id", challenge_id)
        .header("x-avalon-game-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn a_signature_from_the_wrong_key_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = unique_game();

    let register = http
        .post(format!("{base}/games"))
        .json(&game.body)
        .send()
        .await
        .unwrap();
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let key_id = registered["credential"]["key_id"].as_str().unwrap();

    let challenge: serde_json::Value = http
        .post(format!("{base}/games/{slug}/challenge"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();

    // Signed with a key the game never registered.
    let mut csprng = rand::rng();
    let wrong_key = SigningKey::generate(&mut csprng);
    let signature_b64 = BASE64.encode(wrong_key.sign(&nonce).to_bytes());

    let response = http
        .get(format!("{base}/games/whoami"))
        .header("x-avalon-game-key-id", key_id)
        .header("x-avalon-game-challenge-id", challenge_id)
        .header("x-avalon-game-signature", &signature_b64)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

// --- Issue #270: GET /games list + directory read path -------------------

#[tokio::test]
#[ignore]
async fn listing_games_finds_a_freshly_registered_game_by_name_search() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = unique_game();

    let register = http
        .post(format!("{base}/games"))
        .json(&game.body)
        .send()
        .await
        .expect("register game failed — is `make start` running?");
    assert!(register.status().is_success());
    let registered: serde_json::Value = register.json().await.unwrap();
    let slug = registered["slug"].as_str().unwrap();
    let name = registered["name"].as_str().unwrap();

    // No auth header at all — GET /games is public and unauthenticated.
    let response = http
        .get(format!("{base}/games"))
        .query(&[("q", name), ("sort", "name")])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());

    let body: serde_json::Value = response.json().await.unwrap();
    let games = body["games"].as_array().unwrap();
    assert!(games.iter().any(|g| g["slug"].as_str() == Some(slug)));
    let found = games
        .iter()
        .find(|g| g["slug"].as_str() == Some(slug))
        .unwrap();
    assert_eq!(found["name"].as_str().unwrap(), name);
    assert_eq!(found["status"].as_str().unwrap(), "active");
    // Directory listing returns only the public summary fields — no
    // credential/requested_capabilities noise a card doesn't show.
    assert!(found.get("credential").is_none());
    assert!(found.get("requested_capabilities").is_none());
}

#[tokio::test]
#[ignore]
async fn listing_games_paginates_with_cursor_and_next_cursor() {
    let http = reqwest::Client::new();
    let base = server_url();

    for _ in 0..3 {
        let game = unique_game();
        http.post(format!("{base}/games"))
            .json(&game.body)
            .send()
            .await
            .unwrap();
    }

    let first_page: serde_json::Value = http
        .get(format!("{base}/games"))
        .query(&[("sort", "newest"), ("limit", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first_games = first_page["games"].as_array().unwrap();
    assert_eq!(first_games.len(), 1);
    let next_cursor = first_page["next_cursor"].as_str();
    assert!(
        next_cursor.is_some(),
        "expected a next_cursor with 3+ games registered"
    );

    let second_page: serde_json::Value = http
        .get(format!("{base}/games"))
        .query(&[
            ("sort", "newest"),
            ("limit", "1"),
            ("cursor", next_cursor.unwrap()),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_games = second_page["games"].as_array().unwrap();
    assert_eq!(second_games.len(), 1);
    // The second page's game must differ from the first's — the cursor
    // actually advanced rather than re-returning the same row.
    assert_ne!(first_games[0]["id"], second_games[0]["id"]);
}
