//! Exercises guild discovery, search, pagination, integrator breakdown, and pins.
//! Requires a running `avalon-server` and Postgres; tests are `#[ignore]`.
//! See `make test-live` / `make start`.

use sqlx::PgPool;
use uuid::Uuid;

mod common;
mod guild_support;

use guild_support::{create_guild, seed_identity_session, set_recruiting, unique_guild_body};

/// Seeds a `integrators` row directly (bypassing the integrator registration ceremony,
/// same "seed via SQL, endpoint behavior doesn't depend on how the row got
/// there" reasoning `guild_support::seed_identity_session` documents) and
/// returns its id.
async fn seed_integrator(pool: &PgPool, name: &str) -> Uuid {
    let integrator_id = Uuid::new_v4();
    let slug = format!(
        "{}-{}",
        name.to_lowercase().replace(' ', "-"),
        Uuid::new_v4().simple()
    );
    sqlx::query(
        "INSERT INTO integrators (id, slug, name, owner_name, registered_at, status) \
         VALUES ($1, $2, $3, 'test developer', now(), 'active')",
    )
    .bind(integrator_id)
    .bind(&slug[..slug.len().min(64)])
    .bind(name)
    .execute(pool)
    .await
    .expect("failed to seed integrator");
    integrator_id
}

/// Seeds an active `bindings` row directly — see `seed_integrator`'s own note.
async fn seed_binding(
    pool: &PgPool,
    identity_id: avalon_protocol::ids::IdentityId,
    integrator_id: Uuid,
) -> Uuid {
    let binding_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO bindings (id, identity_id, integrator_id, established_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(binding_id)
    .bind(identity_id)
    .bind(integrator_id)
    .execute(pool)
    .await
    .expect("failed to seed binding");
    binding_id
}

async fn end_binding(pool: &PgPool, binding_id: Uuid) {
    sqlx::query("UPDATE bindings SET ended_at = now() WHERE id = $1")
        .bind(binding_id)
        .execute(pool)
        .await
        .expect("failed to end binding");
}

/// Invites `to_identity_id` into `guild_id` and accepts on their behalf,
/// landing them as a plain member (role index 2, no permissions) — the
/// same flow `guild_membership.rs` exercises in `invite_accept_join_and_leave_flow`, reused
/// here just to get a second real guild member without hand-writing an
/// `indexer_guild_members` row.
async fn invite_and_accept(
    http: &reqwest::Client,
    base: &str,
    owner_token: &str,
    to_token: &str,
    guild_id: &str,
    to_identity_id: avalon_protocol::ids::IdentityId,
) {
    let invite = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/invites")),
        owner_token,
    )
    .json(&serde_json::json!({ "to": to_identity_id }))
    .send()
    .await
    .unwrap();
    assert!(invite.status().is_success(), "{:?}", invite.status());
    let invite_body: serde_json::Value = invite.json().await.unwrap();
    let invite_id = invite_body["id"].as_str().unwrap();

    let accept = common::auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
        )),
        to_token,
    )
    .send()
    .await
    .unwrap();
    assert!(accept.status().is_success(), "{:?}", accept.status());
}

/// Ticket #154 acceptance criteria: a recruiting guild is discoverable by a
/// non-member; a non-recruiting guild is excluded from browse results (both
/// the default `recruiting=` omitted case and an explicit
/// `recruiting=false`) for a caller who isn't a member of it, while exact
/// `GET /guilds/{id}` lookup still finds it unchanged, and a member always
/// sees their own non-recruiting guild under `recruiting=false`.
#[tokio::test]
#[ignore]
async fn recruiting_filter_excludes_non_recruiting_guilds_from_general_browse() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_stranger_id, stranger_token) = seed_identity_session(&pool).await;

    let recruiting_guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let recruiting_id = recruiting_guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, recruiting_id, true).await;

    let closed_guild = create_guild(&http, &base, &owner_token, "Closed Guild").await;
    let closed_id = closed_guild["id"].as_str().unwrap();
    // `recruiting` defaults to `false` — no PATCH needed to leave it closed.

    // A stranger's default browse (no `recruiting=` filter) sees the
    // recruiting guild but not the closed one.
    let browse: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/discover")), &stranger_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let ids: Vec<&str> = browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&recruiting_id));
    assert!(!ids.contains(&closed_id));

    // Exact id lookup still finds the closed guild, unchanged.
    let direct = common::auth(
        http.get(format!("{base}/guilds/{closed_id}")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap();
    assert!(direct.status().is_success());

    // `recruiting=false` from a stranger must NOT bulk-leak the closed
    // guild (or any other non-recruiting guild the stranger isn't in) —
    // it stays membership-gated, so a non-member sees nothing here either.
    let closed_browse: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/discover?recruiting=false")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let closed_ids: Vec<&str> = closed_browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(!closed_ids.contains(&closed_id));
    assert!(!closed_ids.contains(&recruiting_id));

    // The owner, who *is* a member of the closed guild, sees it under an
    // explicit `recruiting=false` query.
    let owner_closed_browse: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/discover?recruiting=false")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let owner_closed_ids: Vec<&str> = owner_closed_browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(owner_closed_ids.contains(&closed_id));
    assert!(!owner_closed_ids.contains(&recruiting_id));
}

/// A member of a non-recruiting guild still sees it in their own default
/// (no `recruiting=`) browse results — only strangers lose visibility.
#[tokio::test]
#[ignore]
async fn a_member_still_sees_their_own_non_recruiting_guild_in_default_browse() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let closed_guild = create_guild(&http, &base, &owner_token, "Own Closed Guild").await;
    let closed_id = closed_guild["id"].as_str().unwrap();

    let browse: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/discover")), &owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let ids: Vec<&str> = browse["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&closed_id));
}

/// Issue #258: Discover cards carry `banner`/`icon`, round-tripping the
/// values set via `PATCH /guilds/{id}`, and reporting `null`
/// for a guild that never set them — same shape `GET /guilds/{id}` already
/// returns, just also reachable from the browse listing.
#[tokio::test]
#[ignore]
async fn discover_card_includes_banner_and_icon() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let with_media = create_guild(&http, &base, &owner_token, "Media Guild").await;
    let with_media_id = with_media["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, with_media_id, true).await;
    let patch = common::auth(
        http.patch(format!("{base}/guilds/{with_media_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "banner": "https://example.com/banner.png",
        "icon": "https://example.com/icon.png",
    }))
    .send()
    .await
    .unwrap();
    assert!(patch.status().is_success());

    let without_media = create_guild(&http, &base, &owner_token, "Plain Guild").await;
    let without_media_id = without_media["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, without_media_id, true).await;

    let browse: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/discover")), &owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let guilds = browse["guilds"].as_array().unwrap();

    let with_media_card = guilds
        .iter()
        .find(|g| g["id"].as_str().unwrap() == with_media_id)
        .expect("media guild missing from discover results");
    assert_eq!(
        with_media_card["banner"].as_str().unwrap(),
        "https://example.com/banner.png"
    );
    assert_eq!(
        with_media_card["icon"].as_str().unwrap(),
        "https://example.com/icon.png"
    );

    let without_media_card = guilds
        .iter()
        .find(|g| g["id"].as_str().unwrap() == without_media_id)
        .expect("plain guild missing from discover results");
    assert!(without_media_card["banner"].is_null());
    assert!(without_media_card["icon"].is_null());
}

/// Free-text `q=` matches name/tag substrings case-insensitively.
#[tokio::test]
#[ignore]
async fn search_matches_name_and_tag_substrings_case_insensitively() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let unique_word = format!("Zephyrion{}", &suffix[..6]);
    let body = serde_json::json!({
        "name": format!("{unique_word} Vanguard"),
        "tag": suffix[..5].to_uppercase(),
        "description": "a discover-test guild",
    });
    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success());
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let lowercase_query = unique_word.to_lowercase();
    let search: serde_json::Value = common::auth(
        http.get(format!(
            "{base}/guilds/discover?q={lowercase_query}&recruiting=true"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let ids: Vec<&str> = search["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&guild_id));
}

/// Cursor pagination doesn't skip or duplicate rows across pages.
#[tokio::test]
#[ignore]
async fn pagination_does_not_skip_or_duplicate_rows_across_pages() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;

    let mut created_ids = Vec::new();
    for _ in 0..5 {
        let guild = create_guild(&http, &base, &owner_token, "Paged Guild").await;
        let id = guild["id"].as_str().unwrap().to_string();
        set_recruiting(&http, &base, &owner_token, &id, true).await;
        created_ids.push(id);
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            Some(c) => format!("{base}/guilds/discover?recruiting=true&limit=2&cursor={c}"),
            None => format!("{base}/guilds/discover?recruiting=true&limit=2"),
        };
        let page: serde_json::Value = common::auth(http.get(url), &owner_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let page_ids: Vec<String> = page["guilds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["id"].as_str().unwrap().to_string())
            .collect();
        for id in &page_ids {
            assert!(
                !seen.contains(id),
                "guild {id} appeared on more than one page"
            );
        }
        seen.extend(page_ids);

        cursor = page["next_cursor"].as_str().map(|s| s.to_string());
        if cursor.is_none() {
            break;
        }
    }

    for id in &created_ids {
        assert!(
            seen.contains(id),
            "guild {id} never appeared across any page"
        );
    }
}

/// #206's core acceptance criteria: the breakdown is computed from real
/// `IntegratorBinding` data only, updates as bindings change, and is never
/// something a manager can add for an integrator with zero bound members (there's
/// no add action at all — this test never calls one).
#[tokio::test]
#[ignore]
async fn game_breakdown_reflects_active_bindings_and_updates_as_they_change() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (member_id, member_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    invite_and_accept(
        &http,
        &base,
        &owner_token,
        &member_token,
        guild_id,
        member_id,
    )
    .await;

    let ashen = seed_integrator(&pool, "Ashen Realms").await;
    let ocean = seed_integrator(&pool, "Ocean World").await;

    // Owner and member both play Ashen Realms; only the member plays Ocean
    // World. No integrator association is ever declared — the breakdown must
    // fall entirely out of these bindings.
    let owner_ashen_binding = seed_binding(&pool, owner_id, ashen).await;
    seed_binding(&pool, member_id, ashen).await;
    seed_binding(&pool, member_id, ocean).await;

    let fetch_breakdown = || {
        let http = http.clone();
        let base = base.clone();
        let guild_id = guild_id.to_string();
        let token = owner_token.clone();
        async move {
            common::auth(
                http.get(format!("{base}/guilds/{guild_id}/integrator-breakdown")),
                &token,
            )
            .send()
            .await
            .unwrap()
        }
    };

    let resp = fetch_breakdown().await;
    assert!(resp.status().is_success(), "{:?}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total_members"].as_i64().unwrap(), 2);
    let by_integrator: std::collections::HashMap<String, i64> = body["breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["integrator_name"].as_str().unwrap().to_string(),
                e["member_count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(by_integrator.get("Ashen Realms"), Some(&2));
    assert_eq!(by_integrator.get("Ocean World"), Some(&1));

    // Ending the owner's Ashen Realms binding drops that count to 1 — the
    // breakdown is live-computed, not a stale/cached association.
    end_binding(&pool, owner_ashen_binding).await;

    let resp = fetch_breakdown().await;
    assert!(resp.status().is_success(), "{:?}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    let by_integrator: std::collections::HashMap<String, i64> = body["breakdown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["integrator_name"].as_str().unwrap().to_string(),
                e["member_count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(by_integrator.get("Ashen Realms"), Some(&1));
    assert_eq!(by_integrator.get("Ocean World"), Some(&1));

    // A plain member (no manage_guild) cannot fetch the breakdown while
    // the guild hasn't opted into public exposure.
    let member_view = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/integrator-breakdown")),
        &member_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(member_view.status(), reqwest::StatusCode::FORBIDDEN);
}

/// #206/#160's show/hide toggle: a non-member gets 403 while
/// `game_breakdown_public` is false, and can fetch the same breakdown once
/// a `manage_guild` holder flips it on via `PATCH /guilds/{id}` — the
/// owner's own view is unaffected by the toggle either way, since they can
/// always see it internally.
#[tokio::test]
#[ignore]
async fn game_breakdown_public_toggle_gates_exposure_to_non_members() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_stranger_id, stranger_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();
    assert!(!guild["game_breakdown_public"].as_bool().unwrap());

    let ashen = seed_integrator(&pool, "Ashen Realms").await;
    seed_binding(&pool, owner_id, ashen).await;

    // Not public yet: a non-member (and non-manager) is rejected.
    let before = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/integrator-breakdown")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(before.status(), reqwest::StatusCode::FORBIDDEN);

    // The owner can always see it regardless of the toggle.
    let owner_view = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/integrator-breakdown")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(
        owner_view.status().is_success(),
        "{:?}",
        owner_view.status()
    );

    let patch = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "game_breakdown_public": true }))
    .send()
    .await
    .unwrap();
    assert!(patch.status().is_success(), "{:?}", patch.status());
    let patched: serde_json::Value = patch.json().await.unwrap();
    assert!(patched["game_breakdown_public"].as_bool().unwrap());

    // Now public: the same stranger can fetch it.
    let after = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/integrator-breakdown")),
        &stranger_token,
    )
    .send()
    .await
    .unwrap();
    assert!(after.status().is_success(), "{:?}", after.status());
    let body: serde_json::Value = after.json().await.unwrap();
    assert_eq!(
        body["breakdown"][0]["integrator_name"].as_str().unwrap(),
        "Ashen Realms"
    );
}

/// #207's core round-trip: a `manage_guild` holder (the owner here) pins,
/// reorders, and unpins integrators, always drawing from real #206 affinity data.
/// Also covers the cap and the zero-bound-members rejection at the HTTP
/// layer (unit tests in `crates/server/src/guilds.rs` cover the same
/// invariants against the pure validator directly).
#[tokio::test]
#[ignore]
async fn pin_reorder_and_unpin_round_trip() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let ashen = seed_integrator(&pool, "Ashen Realms").await;
    let ocean = seed_integrator(&pool, "Ocean World").await;
    let unbound = seed_integrator(&pool, "Unbound Integrator").await;

    // The owner is bound to both Ashen Realms and Ocean World — both have
    // real affinity — but never binds to `unbound`.
    seed_binding(&pool, owner_id, ashen).await;
    seed_binding(&pool, owner_id, ocean).await;

    let put_favorites = |integrator_ids: Vec<Uuid>| {
        let http = http.clone();
        let base = base.clone();
        let guild_id = guild_id.to_string();
        let token = owner_token.clone();
        async move {
            common::auth(
                http.put(format!("{base}/guilds/{guild_id}/favorite-integrators")),
                &token,
            )
            .json(&serde_json::json!({ "integrator_ids": integrator_ids }))
            .send()
            .await
            .unwrap()
        }
    };

    // Pinning an integrator with zero bound members is rejected outright.
    let rejected = put_favorites(vec![unbound]).await;
    assert_eq!(rejected.status(), reqwest::StatusCode::FORBIDDEN);

    // Pinning both real-affinity integrators in order succeeds.
    let pinned = put_favorites(vec![ashen, ocean]).await;
    assert!(pinned.status().is_success(), "{:?}", pinned.status());
    let body: serde_json::Value = pinned.json().await.unwrap();
    let names: Vec<&str> = body["favorites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["integrator_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ashen Realms", "Ocean World"]);
    assert!(!body["favorites"][0]["stale"].as_bool().unwrap());

    // Reordering (Ocean World first) round-trips through both the PUT
    // response and a subsequent GET.
    let reordered = put_favorites(vec![ocean, ashen]).await;
    assert!(reordered.status().is_success(), "{:?}", reordered.status());
    let body: serde_json::Value = reordered.json().await.unwrap();
    let names: Vec<&str> = body["favorites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["integrator_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ocean World", "Ashen Realms"]);

    let get = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/favorite-integrators")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(get.status().is_success(), "{:?}", get.status());
    let get_body: serde_json::Value = get.json().await.unwrap();
    let names: Vec<&str> = get_body["favorites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["integrator_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ocean World", "Ashen Realms"]);

    // The guild's public profile carries the same ordered favorites.
    let profile = http
        .get(format!("{base}/guilds/{guild_id}"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    let profile_body: serde_json::Value = profile.json().await.unwrap();
    let names: Vec<&str> = profile_body["favorite_games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["integrator_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ocean World", "Ashen Realms"]);

    // Unpinning: an empty list clears everything.
    let cleared = put_favorites(vec![]).await;
    assert!(cleared.status().is_success(), "{:?}", cleared.status());
    let body: serde_json::Value = cleared.json().await.unwrap();
    assert!(body["favorites"].as_array().unwrap().is_empty());
}

/// A 6th pin is rejected server-side even if the caller has real affinity
/// for all six integrators.
#[tokio::test]
#[ignore]
async fn a_sixth_pin_is_rejected_over_http() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let mut integrator_ids = Vec::new();
    for i in 0..6 {
        let integrator_id = seed_integrator(&pool, &format!("Integrator {i}")).await;
        seed_binding(&pool, owner_id, integrator_id).await;
        integrator_ids.push(integrator_id);
    }

    let resp = common::auth(
        http.put(format!("{base}/guilds/{guild_id}/favorite-integrators")),
        &owner_token,
    )
    .json(&serde_json::json!({ "integrator_ids": integrator_ids }))
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// #207's staleness invariant: when a pinned integrator's last bound member
/// unbinds, the pin is NOT auto-removed, but the read response flags it
/// `stale: true` so a `manage_guild` holder can choose to unpin it.
#[tokio::test]
#[ignore]
async fn a_stale_pin_is_flagged_but_not_auto_removed() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let guild: serde_json::Value = create.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let ashen = seed_integrator(&pool, "Ashen Realms").await;
    let binding_id = seed_binding(&pool, owner_id, ashen).await;

    let pin = common::auth(
        http.put(format!("{base}/guilds/{guild_id}/favorite-integrators")),
        &owner_token,
    )
    .json(&serde_json::json!({ "integrator_ids": [ashen] }))
    .send()
    .await
    .unwrap();
    assert!(pin.status().is_success(), "{:?}", pin.status());

    // The owner's last (only) binding to Ashen Realms ends — the pin must
    // survive this untouched, just flagged stale.
    end_binding(&pool, binding_id).await;

    let get = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/favorite-integrators")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(get.status().is_success(), "{:?}", get.status());
    let body: serde_json::Value = get.json().await.unwrap();
    let favorites = body["favorites"].as_array().unwrap();
    assert_eq!(favorites.len(), 1, "the stale pin must not be auto-removed");
    assert!(favorites[0]["stale"].as_bool().unwrap());
}
