//! Matrix test for issue #87's `Visibility` scope model against a real,
//! running `avalon-server` and Postgres. Gated `--ignored` since it needs
//! live infra — see `make test-live` / `make start`.
//!
//! Test identities/friendships are seeded directly via SQL — same approach
//! `crates/server/tests/presence.rs`/`friends.rs` already use, since
//! neither presence nor friendship-graph reads care how a session was
//! established. Guilds are created through the real `POST /guilds`
//! endpoint (id generation, owner assignment, and validation all live
//! there); membership beyond the owner is seeded directly, the same
//! shortcut `guild_members` rows already get in this test suite's sibling
//! files.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
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
        .bind(format!("vis-test-{identity_id}"))
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

async fn seed_friendship(pool: &PgPool, x: Uuid, y: Uuid) {
    let (a, b) = if x < y { (x, y) } else { (y, x) };
    sqlx::query("INSERT INTO friendships (a, b) VALUES ($1, $2)")
        .bind(a)
        .bind(b)
        .execute(pool)
        .await
        .expect("failed to seed friendship");
}

fn auth(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.bearer_auth(token)
}

async fn set_presence_visibility(http: &reqwest::Client, base: &str, token: &str, value: &str) {
    let response = auth(http.patch(format!("{base}/me")), token)
        .json(&serde_json::json!({ "presence_visibility": value }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(status.is_success(), "{status:?}: {body}");
}

async fn publish_presence(http: &reqwest::Client, base: &str, token: &str) {
    let response = auth(http.put(format!("{base}/me/presence")), token)
        .json(&serde_json::json!({ "status": "Online" }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(status.is_success(), "{status:?}: {body}");
}

/// `true` if `viewer_token` sees `subject`'s real (`online`) presence via
/// `GET /presence`; `false` if it reads as the "not visible" fallback
/// (`offline`, indistinguishable from a genuinely missing/expired entry —
/// issue #97's own posture, reused here).
async fn presence_visible_to(
    http: &reqwest::Client,
    base: &str,
    viewer_token: &str,
    subject: Uuid,
) -> bool {
    let response: serde_json::Value = auth(http.get(format!("{base}/presence")), viewer_token)
        .query(&[("ids", subject.to_string())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    response[0]["status"] == "Online"
}

#[tokio::test]
#[ignore]
async fn presence_visibility_matrix() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (subject_id, subject_token) = seed_identity_session(&pool).await;
    let (friend_id, friend_token) = seed_identity_session(&pool).await;
    let (stranger_id, stranger_token) = seed_identity_session(&pool).await;
    let _ = friend_id;
    let _ = stranger_id;
    seed_friendship(&pool, subject_id, friend_id).await;
    publish_presence(&http, &base, &subject_token).await;

    // Self always sees their own real presence, regardless of setting.
    set_presence_visibility(&http, &base, &subject_token, "private").await;
    assert!(presence_visible_to(&http, &base, &subject_token, subject_id).await);

    // Friends (the default): friend sees it, stranger doesn't.
    set_presence_visibility(&http, &base, &subject_token, "friends").await;
    assert!(presence_visible_to(&http, &base, &friend_token, subject_id).await);
    assert!(!presence_visible_to(&http, &base, &stranger_token, subject_id).await);

    // Public: both see it.
    set_presence_visibility(&http, &base, &subject_token, "public").await;
    assert!(presence_visible_to(&http, &base, &friend_token, subject_id).await);
    assert!(presence_visible_to(&http, &base, &stranger_token, subject_id).await);

    // Private: neither sees it, not even the friend.
    set_presence_visibility(&http, &base, &subject_token, "private").await;
    assert!(!presence_visible_to(&http, &base, &friend_token, subject_id).await);
    assert!(!presence_visible_to(&http, &base, &stranger_token, subject_id).await);
}

async fn create_guild(http: &reqwest::Client, base: &str, owner_token: &str) -> Uuid {
    let suffix = Uuid::new_v4().simple().to_string();
    let response: serde_json::Value = auth(http.post(format!("{base}/guilds")), owner_token)
        .json(&serde_json::json!({
            "name": format!("Visibility Test {}", &suffix[..8]),
            "tag": suffix[..4].to_uppercase(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    response["id"].as_str().unwrap().parse().unwrap()
}

async fn seed_guild_membership(pool: &PgPool, guild_id: Uuid, identity_id: Uuid) {
    sqlx::query("INSERT INTO guild_members (guild_id, identity_id, role_index) VALUES ($1, $2, 1)")
        .bind(guild_id)
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed guild membership");
}

async fn set_roster_visibility(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    guild_id: Uuid,
    value: &str,
) {
    let response = auth(http.patch(format!("{base}/guilds/{guild_id}")), owner_token)
        .json(&serde_json::json!({ "roster_visibility": value }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

async fn can_list_members(http: &reqwest::Client, base: &str, token: &str, guild_id: Uuid) -> bool {
    let response = auth(http.get(format!("{base}/guilds/{guild_id}/members")), token)
        .send()
        .await
        .unwrap();
    response.status().is_success()
}

#[tokio::test]
#[ignore]
async fn guild_roster_visibility_matrix() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;
    let (outsider_id, outsider_token) = seed_identity_session(&pool).await;
    let _ = outsider_id;

    let guild_id = create_guild(&http, &base, &owner_token).await;
    seed_guild_membership(&pool, guild_id, member_id).await;
    let _ = owner_id;

    // guild_members (the default): both the owner and the seeded member
    // see it, the outsider doesn't.
    set_roster_visibility(&http, &base, &owner_token, guild_id, "guild_members").await;
    assert!(can_list_members(&http, &base, &owner_token, guild_id).await);
    assert!(can_list_members(&http, &base, &member_token, guild_id).await);
    assert!(!can_list_members(&http, &base, &outsider_token, guild_id).await);

    // Public: everyone sees it.
    set_roster_visibility(&http, &base, &owner_token, guild_id, "public").await;
    assert!(can_list_members(&http, &base, &outsider_token, guild_id).await);

    // Private: the owner still sees it (a private roster setting gates
    // *outside* exposure, never the guild's own management out of its own
    // roster — `list_members`'s own doc comment), but a regular member and
    // an outsider are both excluded.
    set_roster_visibility(&http, &base, &owner_token, guild_id, "private").await;
    assert!(can_list_members(&http, &base, &owner_token, guild_id).await);
    assert!(!can_list_members(&http, &base, &member_token, guild_id).await);
    assert!(!can_list_members(&http, &base, &outsider_token, guild_id).await);
}

async fn set_guild_public(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    guild_id: Uuid,
    value: bool,
) {
    let response = auth(http.patch(format!("{base}/guilds/{guild_id}")), owner_token)
        .json(&serde_json::json!({ "public": value }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

async fn set_recruiting(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    guild_id: Uuid,
    value: bool,
) {
    let response = auth(http.patch(format!("{base}/guilds/{guild_id}")), owner_token)
        .json(&serde_json::json!({ "recruiting": value }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
}

/// A `public` guild's roster is visible to any authenticated identity
/// regardless of `roster_visibility`, independent of `recruiting` (issue
/// #449, decided: these are separate settings — see
/// `crates/server/src/guilds.rs::list_members`). Turning `public` off
/// restores whatever `roster_visibility` was already set to.
#[tokio::test]
#[ignore]
async fn a_public_guilds_roster_is_visible_despite_a_private_roster_setting() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;

    let guild_id = create_guild(&http, &base, &owner_token).await;
    set_roster_visibility(&http, &base, &owner_token, guild_id, "private").await;
    assert!(
        !can_list_members(&http, &base, &outsider_token, guild_id).await,
        "a non-public private guild must stay hidden from an outsider"
    );

    set_guild_public(&http, &base, &owner_token, guild_id, true).await;
    assert!(
        can_list_members(&http, &base, &outsider_token, guild_id).await,
        "public must override roster_visibility for an outsider"
    );

    set_guild_public(&http, &base, &owner_token, guild_id, false).await;
    assert!(
        !can_list_members(&http, &base, &outsider_token, guild_id).await,
        "turning public back off must restore the private setting"
    );
}

/// #449's whole point: `recruiting` and `public` are independent. A guild
/// can recruit (discovery board + join requests) without exposing its
/// roster, and can expose its roster without actively recruiting.
#[tokio::test]
#[ignore]
async fn recruiting_and_public_are_independent_settings() {
    let http = reqwest::Client::new();
    let base = server_url();
    let pool = test_pool().await;

    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_outsider_id, outsider_token) = seed_identity_session(&pool).await;

    let guild_id = create_guild(&http, &base, &owner_token).await;
    set_roster_visibility(&http, &base, &owner_token, guild_id, "private").await;

    // Recruiting alone no longer implies roster visibility.
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;
    assert!(
        !can_list_members(&http, &base, &outsider_token, guild_id).await,
        "recruiting alone must not expose a private roster post-#449"
    );

    // Public alone, without recruiting, still exposes the roster.
    set_recruiting(&http, &base, &owner_token, guild_id, false).await;
    set_guild_public(&http, &base, &owner_token, guild_id, true).await;
    assert!(
        can_list_members(&http, &base, &outsider_token, guild_id).await,
        "public alone (no recruiting) must still expose the roster"
    );
}
