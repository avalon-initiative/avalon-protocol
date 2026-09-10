//! Exercises `AchievementDefinition` CRUD per game (issue #31) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`. Skipped in this
//! sandbox per `.claude/CLAUDE.md` (no reachable Postgres here); written but
//! not run against a live database.
//!
//! Mirrors `crates/server/tests/games.rs`'s own pattern for the
//! challenge-response game-auth flow: each game-authenticated request needs
//! its own fresh, single-use challenge, so [`game_auth_headers`] mints one
//! per call rather than caching headers across requests.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::header::HeaderMap;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

struct RegisteredGame {
    signing_key: SigningKey,
    slug: String,
    key_id: String,
}

async fn register_game(http: &reqwest::Client, base: &str) -> RegisteredGame {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut csprng = rand::rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let slug = format!("test-ach-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": format!("Test Game {}", &suffix[..8]),
        "developer": "Test Studio",
        "requested_capabilities": [],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": BASE64.encode(signing_key.verifying_key().as_bytes()),
        },
    });

    let response = http
        .post(format!("{base}/games"))
        .json(&body)
        .send()
        .await
        .expect("register game failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let registered: serde_json::Value = response.json().await.unwrap();
    let key_id = registered["credential"]["key_id"]
        .as_str()
        .unwrap()
        .to_string();

    RegisteredGame {
        signing_key,
        slug,
        key_id,
    }
}

/// Mints a fresh challenge for `game` and signs it, returning the three
/// `x-avalon-game-*` headers `games::authenticate_game` verifies — a new
/// set every call, since a challenge is single-use.
async fn game_auth_headers(http: &reqwest::Client, base: &str, game: &RegisteredGame) -> HeaderMap {
    let challenge: serde_json::Value = http
        .post(format!("{base}/games/{}/challenge", game.slug))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge_id = challenge["challenge_id"].as_str().unwrap();
    let nonce = BASE64.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let signature = game.signing_key.sign(&nonce);

    let mut headers = HeaderMap::new();
    headers.insert("x-avalon-game-key-id", game.key_id.parse().unwrap());
    headers.insert("x-avalon-game-challenge-id", challenge_id.parse().unwrap());
    headers.insert(
        "x-avalon-game-signature",
        BASE64.encode(signature.to_bytes()).parse().unwrap(),
    );
    headers
}

#[tokio::test]
#[ignore]
async fn two_games_defining_the_same_key_both_succeed_with_distinct_ids() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game_a = register_game(&http, &base).await;
    let game_b = register_game(&http, &base).await;

    let body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });

    let headers_a = game_auth_headers(&http, &base, &game_a).await;
    let response_a = http
        .post(format!("{base}/games/{}/achievements", game_a.slug))
        .headers(headers_a)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        response_a.status().is_success(),
        "{:?}",
        response_a.status()
    );
    let def_a: serde_json::Value = response_a.json().await.unwrap();

    let headers_b = game_auth_headers(&http, &base, &game_b).await;
    let response_b = http
        .post(format!("{base}/games/{}/achievements", game_b.slug))
        .headers(headers_b)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        response_b.status().is_success(),
        "{:?}",
        response_b.status()
    );
    let def_b: serde_json::Value = response_b.json().await.unwrap();

    assert_ne!(def_a["id"], def_b["id"]);
    assert_eq!(
        def_a["id"].as_str().unwrap(),
        format!("game:{}:achievement:dragon_slayer", game_a.slug)
    );
    assert_eq!(
        def_b["id"].as_str().unwrap(),
        format!("game:{}:achievement:dragon_slayer", game_b.slug)
    );
    assert_eq!(def_a["version"], 1);
}

#[tokio::test]
#[ignore]
async fn defining_a_duplicate_key_for_the_same_game_conflicts() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = register_game(&http, &base).await;

    let body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });

    let headers = game_auth_headers(&http, &base, &game).await;
    let first = http
        .post(format!("{base}/games/{}/achievements", game.slug))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());

    let headers = game_auth_headers(&http, &base, &game).await;
    let second = http
        .post(format!("{base}/games/{}/achievements", game.slug))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore]
async fn a_game_defining_under_another_slug_is_forbidden() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game_a = register_game(&http, &base).await;
    let game_b = register_game(&http, &base).await;

    let body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });

    // game_a's real credential, but the path names game_b's slug.
    let headers = game_auth_headers(&http, &base, &game_a).await;
    let response = http
        .post(format!("{base}/games/{}/achievements", game_b.slug))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn updating_a_definition_bumps_version_and_leaves_the_id_unchanged() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = register_game(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });
    let headers = game_auth_headers(&http, &base, &game).await;
    let created: serde_json::Value = http
        .post(format!("{base}/games/{}/achievements", game.slug))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["version"], 1);

    let update_body = serde_json::json!({
        "description": "Slew the Dragon Lord in single combat",
    });
    let headers = game_auth_headers(&http, &base, &game).await;
    let updated_response = http
        .patch(format!(
            "{base}/games/{}/achievements/dragon_slayer",
            game.slug
        ))
        .headers(headers)
        .json(&update_body)
        .send()
        .await
        .unwrap();
    assert!(updated_response.status().is_success());
    let updated: serde_json::Value = updated_response.json().await.unwrap();

    assert_eq!(updated["id"], created["id"]);
    assert_eq!(updated["version"], 2);
    assert_eq!(
        updated["description"].as_str().unwrap(),
        "Slew the Dragon Lord in single combat"
    );
}

#[tokio::test]
#[ignore]
async fn listing_a_games_achievements_is_public_and_unauthenticated() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = register_game(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });
    let headers = game_auth_headers(&http, &base, &game).await;
    http.post(format!("{base}/games/{}/achievements", game.slug))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap();

    // No auth headers at all — this is a public read.
    let list_response = http
        .get(format!("{base}/games/{}/achievements", game.slug))
        .send()
        .await
        .unwrap();
    assert!(list_response.status().is_success());
    let list: Vec<serde_json::Value> = list_response.json().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["key"], "dragon_slayer");
    assert_eq!(list[0]["retired"], false);
}

#[tokio::test]
#[ignore]
async fn retiring_a_definition_marks_it_retired_without_deleting_it() {
    let http = reqwest::Client::new();
    let base = server_url();
    let game = register_game(&http, &base).await;

    let create_body = serde_json::json!({
        "key": "dragon_slayer",
        "name": "Dragon Slayer",
        "description": "Slew the dragon",
    });
    let headers = game_auth_headers(&http, &base, &game).await;
    http.post(format!("{base}/games/{}/achievements", game.slug))
        .headers(headers)
        .json(&create_body)
        .send()
        .await
        .unwrap();

    let headers = game_auth_headers(&http, &base, &game).await;
    let retire_response = http
        .patch(format!(
            "{base}/games/{}/achievements/dragon_slayer",
            game.slug
        ))
        .headers(headers)
        .json(&serde_json::json!({ "retired": true }))
        .send()
        .await
        .unwrap();
    assert!(retire_response.status().is_success());
    let retired: serde_json::Value = retire_response.json().await.unwrap();
    assert_eq!(retired["retired"], true);
    // Retiring never changes the definition or version.
    assert_eq!(retired["version"], 1);

    let list_response = http
        .get(format!("{base}/games/{}/achievements", game.slug))
        .send()
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = list_response.json().await.unwrap();
    assert_eq!(list.len(), 1, "retiring never deletes the definition");
    assert_eq!(list[0]["retired"], true);
}
