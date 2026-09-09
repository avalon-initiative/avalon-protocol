//! Exercises `GET /people/discover` (issue #204) against a real, running
//! `avalon-server` and Postgres. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`. Same seeding approach as
//! `crates/server/tests/friends.rs`/`crates/server/tests/blocks.rs`:
//! identities and sessions are inserted directly rather than through a
//! real WebAuthn ceremony.

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

async fn seed_identity_session(pool: &PgPool) -> (Uuid, String) {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("discovery-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");

    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");

    (identity_id, token)
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

/// Sends a friend request from `from_token` to `to_id` and accepts it as
/// `to_token`, leaving the two as real, durable friends — same two-call
/// flow `crates/server/tests/friends.rs` exercises directly.
async fn become_friends(
    http: &reqwest::Client,
    base: &str,
    from_token: &str,
    to_id: Uuid,
    to_token: &str,
) {
    let create = auth(http.post(format!("{base}/friends/requests")), from_token)
        .json(&serde_json::json!({ "to": to_id }))
        .send()
        .await
        .expect("create friend request failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let body: serde_json::Value = create.json().await.unwrap();
    let request_id = body["id"].as_str().unwrap();

    let accept = auth(
        http.post(format!("{base}/friends/requests/{request_id}/accept")),
        to_token,
    )
    .send()
    .await
    .unwrap();
    assert!(accept.status().is_success(), "{:?}", accept.status());
}

fn unique_guild_body() -> serde_json::Value {
    let suffix = Uuid::new_v4().simple().to_string();
    serde_json::json!({
        "name": format!("Discovery Test Guild {}", &suffix[..8]),
        "tag": suffix[..4].to_uppercase(),
        "description": "a discovery-test guild",
    })
}

/// Creates a guild as `owner_token` and adds `member_id`/`member_token` to
/// it via the invite→accept flow (`crates/server/tests/guilds.rs`'s
/// `invite_accept_join_and_leave_flow` precedent), returning the guild id.
async fn create_guild_with_member(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    member_id: Uuid,
    member_token: &str,
) -> String {
    let create = auth(http.post(format!("{base}/guilds")), owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    assert!(create.status().is_success(), "{:?}", create.status());
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap().to_string();

    let invite = auth(
        http.post(format!("{base}/guilds/{guild_id}/invites")),
        owner_token,
    )
    .json(&serde_json::json!({ "to": member_id }))
    .send()
    .await
    .unwrap();
    assert!(invite.status().is_success(), "{:?}", invite.status());
    let invite_body: serde_json::Value = invite.json().await.unwrap();
    let invite_id = invite_body["id"].as_str().unwrap();

    let accept = auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
        )),
        member_token,
    )
    .send()
    .await
    .unwrap();
    assert!(accept.status().is_success(), "{:?}", accept.status());

    guild_id
}

async fn discover(http: &reqwest::Client, base: &str, token: &str) -> Vec<String> {
    let response = auth(http.get(format!("{base}/people/discover")), token)
        .send()
        .await
        .expect("discover request failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    body["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["identity_id"].as_str().unwrap().to_string())
        .collect()
}

/// Core friends-of-friends acceptance criterion: alice-bob and bob-carol
/// are both friendships, alice and carol are not — carol must surface as
/// a candidate for alice, and bob (already a friend) must not.
#[tokio::test]
#[ignore]
async fn a_friend_of_a_friend_surfaces_as_a_candidate() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;
    let (carol_id, carol_token) = seed_identity_session(&pool).await;

    become_friends(&http, &base, &alice_token, bob_id, &bob_token).await;
    become_friends(&http, &base, &bob_token, carol_id, &carol_token).await;

    let candidates = discover(&http, &base, &alice_token).await;
    assert!(
        candidates.contains(&carol_id.to_string()),
        "expected carol (friend of bob) to surface for alice: {candidates:?}"
    );
    assert!(
        !candidates.contains(&bob_id.to_string()),
        "bob is already alice's friend and must not surface as a suggestion: {candidates:?}"
    );
    assert!(
        !candidates.contains(&alice_id.to_string()),
        "alice must never surface as her own candidate: {candidates:?}"
    );
}

/// Core mutual-guild acceptance criterion: alice and bob share a guild,
/// alice and bob are not friends — bob must surface as a candidate for
/// alice.
#[tokio::test]
#[ignore]
async fn a_mutual_guild_member_surfaces_as_a_candidate() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    create_guild_with_member(&http, &base, &alice_token, bob_id, &bob_token).await;

    let candidates = discover(&http, &base, &alice_token).await;
    assert!(
        candidates.contains(&bob_id.to_string()),
        "expected bob (shared guild membership) to surface for alice: {candidates:?}"
    );
}

/// Core invariant: a block hides a candidate in either direction, even
/// when the underlying relationship (here, mutual guild membership) would
/// otherwise surface them — mirrors presence's own "a block hides even
/// among friends" test in `crates/server/tests/presence.rs`.
#[tokio::test]
#[ignore]
async fn a_blocked_identity_never_surfaces_even_via_mutual_guild() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (_alice_id, alice_token) = seed_identity_session(&pool).await;
    let (bob_id, bob_token) = seed_identity_session(&pool).await;

    create_guild_with_member(&http, &base, &alice_token, bob_id, &bob_token).await;

    let block = auth(http.post(format!("{base}/blocks")), &alice_token)
        .json(&serde_json::json!({ "identity_id": bob_id }))
        .send()
        .await
        .unwrap();
    assert!(block.status().is_success(), "{:?}", block.status());

    let candidates = discover(&http, &base, &alice_token).await;
    assert!(
        !candidates.contains(&bob_id.to_string()),
        "a blocked identity must never surface as a discovery candidate: {candidates:?}"
    );

    // Direction-agnostic: bob having blocked alice must equally hide alice
    // from *bob's* own discover results, even though bob never opts in.
    let bob_candidates = discover(&http, &base, &bob_token).await;
    assert!(!bob_candidates.contains(&_alice_id.to_string()));
}

/// Discovery is session-authenticated only — no query parameter of any
/// kind changes the result, matching the ticket's explicit "never a
/// name/handle search" invariant.
#[tokio::test]
#[ignore]
async fn discover_requires_a_session_token() {
    let http = reqwest::Client::new();
    let base = server_url();

    let response = http
        .get(format!("{base}/people/discover"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

// --- #205: opt-in global name/handle search -----------------------------

/// `PATCH /me` with `{"discoverable": true|false}`.
async fn set_discoverable(http: &reqwest::Client, base: &str, token: &str, discoverable: bool) {
    let response = auth(http.patch(format!("{base}/me")), token)
        .json(&serde_json::json!({ "discoverable": discoverable }))
        .send()
        .await
        .expect("PATCH /me failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["discoverable"], discoverable);
}

async fn search(http: &reqwest::Client, base: &str, token: &str, q: &str) -> Vec<String> {
    let response = auth(http.get(format!("{base}/identities/search")), token)
        .query(&[("q", q)])
        .send()
        .await
        .expect("search request failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["identity_id"].as_str().unwrap().to_string())
        .collect()
}

/// Fetches an identity's own `display_name` (needed to build a query term
/// that will actually match it, since `seed_identity_session` gives each
/// identity a unique random-ish display name).
async fn display_name(pool: &PgPool, identity_id: Uuid) -> String {
    let row = sqlx::query("SELECT display_name FROM profiles WHERE identity_id = $1")
        .bind(identity_id)
        .fetch_one(pool)
        .await
        .unwrap();
    row.try_get::<String, _>("display_name").unwrap()
}

/// Core acceptance criterion: off by default, opting in makes an identity
/// searchable by a substring of its display name, opting back out removes
/// it again immediately (no grace period) — the full round trip #205
/// promises.
#[tokio::test]
#[ignore]
async fn opting_in_then_out_of_search_takes_effect_immediately() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (target_id, target_token) = seed_identity_session(&pool).await;
    let (_searcher_id, searcher_token) = seed_identity_session(&pool).await;
    let name = display_name(&pool, target_id).await;
    let q = &name[..name.len().min(12)];

    // Off by default: not found before ever toggling.
    let before = search(&http, &base, &searcher_token, q).await;
    assert!(
        !before.contains(&target_id.to_string()),
        "an identity must never be searchable before opting in: {before:?}"
    );

    // Opt in: now searchable.
    set_discoverable(&http, &base, &target_token, true).await;
    let during = search(&http, &base, &searcher_token, q).await;
    assert!(
        during.contains(&target_id.to_string()),
        "expected the opted-in identity to appear in search: {during:?}"
    );

    // Opt out: gone again, immediately (this same call, no polling/retry).
    set_discoverable(&http, &base, &target_token, false).await;
    let after = search(&http, &base, &searcher_token, q).await;
    assert!(
        !after.contains(&target_id.to_string()),
        "an opted-out identity must disappear from search immediately: {after:?}"
    );
}

/// A non-opted-in identity never appears in search, even to a caller who
/// searches its exact full handle (`display_name#discriminator`) — that
/// exact-match path is `GET /friends/handle/:handle`, deliberately
/// untouched and separate from this fuzzy endpoint.
#[tokio::test]
#[ignore]
async fn a_non_opted_in_identity_never_appears_even_via_its_exact_handle() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (target_id, _target_token) = seed_identity_session(&pool).await;
    let (_searcher_id, searcher_token) = seed_identity_session(&pool).await;
    let name = display_name(&pool, target_id).await;

    let results = search(&http, &base, &searcher_token, &name).await;
    assert!(
        !results.contains(&target_id.to_string()),
        "a non-opted-in identity must never appear in search, even by exact name: {results:?}"
    );
}

/// A block hides an opted-in identity from search in either direction,
/// same invariant `a_blocked_identity_never_surfaces_even_via_mutual_guild`
/// exercises for `GET /people/discover`.
#[tokio::test]
#[ignore]
async fn a_blocked_searcher_gets_no_result_even_when_the_target_is_opted_in() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (target_id, target_token) = seed_identity_session(&pool).await;
    let (_blocker_id, blocker_token) = seed_identity_session(&pool).await;
    let name = display_name(&pool, target_id).await;
    let q = &name[..name.len().min(12)];

    set_discoverable(&http, &base, &target_token, true).await;

    // The searcher blocks the target.
    let block = auth(http.post(format!("{base}/blocks")), &blocker_token)
        .json(&serde_json::json!({ "identity_id": target_id }))
        .send()
        .await
        .unwrap();
    assert!(block.status().is_success(), "{:?}", block.status());

    let results = search(&http, &base, &blocker_token, q).await;
    assert!(
        !results.contains(&target_id.to_string()),
        "a blocked identity must not appear in the blocker's search results: {results:?}"
    );
}

/// Search is session-authenticated, same as every other route in this
/// module.
#[tokio::test]
#[ignore]
async fn search_requires_a_session_token() {
    let http = reqwest::Client::new();
    let base = server_url();

    let response = http
        .get(format!("{base}/identities/search?q=alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// `discoverable` must never flip as a side effect of a `PATCH /me` that
/// ultimately fails on an unrelated field — a default-off, no-exceptions
/// preference must have no partial-write path. `avatar_url` is deliberately
/// invalid here so the request is guaranteed to fail validation; if
/// `set_discoverable` ran before that validation, `discoverable` would end
/// up `true` anyway despite the caller seeing an error response.
#[tokio::test]
#[ignore]
async fn discoverable_does_not_flip_when_the_rest_of_the_patch_fails() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (identity_id, token) = seed_identity_session(&pool).await;

    let response = auth(http.patch(format!("{base}/me")), &token)
        .json(&serde_json::json!({
            "discoverable": true,
            "avatar_url": "not-a-valid-url",
        }))
        .send()
        .await
        .unwrap();
    assert!(
        !response.status().is_success(),
        "expected the invalid avatar_url to fail this request: {:?}",
        response.status()
    );

    let discoverable: bool =
        sqlx::query("SELECT discoverable FROM discovery_preferences WHERE identity_id = $1")
            .bind(identity_id)
            .fetch_optional(&pool)
            .await
            .unwrap()
            .map(|row| row.try_get("discoverable").unwrap())
            .unwrap_or(false);
    assert!(
        !discoverable,
        "discoverable must stay false when the rest of the PATCH /me request failed"
    );
}
