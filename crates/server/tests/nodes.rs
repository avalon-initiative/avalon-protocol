//! Exercises node-to-node announce/bootstrap discovery (issue #362) against
//! a real, running `avalon-server`. Gated `--ignored` since it needs live
//! infra — see `make test-live` / `make start`.
//!
//! Most of this drives `POST /nodes/announce`/`GET /nodes/peers` directly
//! from the test rather than waiting on `nodes::run_worker`'s own timer —
//! same convention `tests/mirror_watcher.rs` already establishes for
//! testing a background worker's underlying mechanism without depending on
//! its poll interval.
//!
//! The real two-node round trip (`two_nodes_see_each_other_via_announce`)
//! needs a second `avalon-server` process — set
//! `AVALON_SECOND_NODE_SERVER_URL` to run it; it's skipped (not failed)
//! when unset, same pattern `tests/remote_settlement.rs` (#313) already
//! uses for its own second-node scenario.

use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn network_id() -> String {
    std::env::var("AVALON_NETWORK_ID").unwrap_or_else(|_| "avalon-dev-local".to_string())
}

fn second_node_url() -> Option<String> {
    std::env::var("AVALON_SECOND_NODE_SERVER_URL").ok()
}

/// `protocol_version` defaults to the running crate's own version (real
/// semver, guaranteed to clear the target's floor) — see
/// `announce_with_version` for the version-gating-specific tests, which
/// need to control this explicitly.
async fn announce(
    http: &reqwest::Client,
    target_base: &str,
    caller_base_url: &str,
    network_id: &str,
) -> reqwest::Response {
    announce_with_version(
        http,
        target_base,
        caller_base_url,
        network_id,
        avalon_server::version::PROTOCOL_VERSION,
    )
    .await
}

async fn announce_with_version(
    http: &reqwest::Client,
    target_base: &str,
    caller_base_url: &str,
    network_id: &str,
    protocol_version: &str,
) -> reqwest::Response {
    http.post(format!("{target_base}/nodes/announce"))
        .json(&serde_json::json!({
            "base_url": caller_base_url,
            "roles": ["combined"],
            "protocol_version": protocol_version,
            "network_id": network_id,
        }))
        .send()
        .await
        .expect("POST /nodes/announce failed — is `make start` running?")
}

async fn list_peers(http: &reqwest::Client, target_base: &str) -> Vec<serde_json::Value> {
    http.get(format!("{target_base}/nodes/peers"))
        .send()
        .await
        .expect("GET /nodes/peers failed")
        .json()
        .await
        .expect("GET /nodes/peers response was not JSON")
}

#[tokio::test]
#[ignore]
async fn announcing_upserts_the_caller_and_the_response_excludes_it() {
    let http = reqwest::Client::new();
    let base = server_url();
    let caller_base_url = format!("http://test-harness-{}.invalid", Uuid::new_v4());

    let response = announce(&http, &base, &caller_base_url, &network_id()).await;
    assert!(response.status().is_success(), "{:?}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    let returned_peers = body["peers"].as_array().unwrap();
    assert!(
        !returned_peers
            .iter()
            .any(|p| p["base_url"] == caller_base_url),
        "the announce response must never echo the caller's own entry back to it"
    );

    let peers = list_peers(&http, &base).await;
    assert!(
        peers.iter().any(|p| p["base_url"] == caller_base_url),
        "the announced peer must now appear in GET /nodes/peers"
    );
}

#[tokio::test]
#[ignore]
async fn announcing_the_same_peer_twice_refreshes_not_duplicates() {
    let http = reqwest::Client::new();
    let base = server_url();
    let caller_base_url = format!("http://test-harness-{}.invalid", Uuid::new_v4());

    announce(&http, &base, &caller_base_url, &network_id()).await;
    announce(&http, &base, &caller_base_url, &network_id()).await;

    let peers = list_peers(&http, &base).await;
    let matches = peers
        .iter()
        .filter(|p| p["base_url"] == caller_base_url)
        .count();
    assert_eq!(matches, 1, "re-announcing must refresh, never duplicate");
}

#[tokio::test]
#[ignore]
async fn an_announcement_naming_a_different_network_id_is_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let caller_base_url = format!("http://test-harness-{}.invalid", Uuid::new_v4());

    let response = announce(
        &http,
        &base,
        &caller_base_url,
        "some-other-network-entirely",
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);

    let peers = list_peers(&http, &base).await;
    assert!(
        !peers.iter().any(|p| p["base_url"] == caller_base_url),
        "a mismatched-network_id announcement must never be added to the peer table"
    );
}

/// Issue #368: unlike a `network_id` mismatch, a below-floor version
/// doesn't reject the request outright — it just isn't added to the peer
/// table. Also proves the exclusion is reversible: the same caller
/// re-announcing with a real version is admitted normally right after.
#[tokio::test]
#[ignore]
async fn an_announcement_below_the_version_floor_is_excluded_but_not_rejected() {
    let http = reqwest::Client::new();
    let base = server_url();
    let net = network_id();
    let caller_base_url = format!("http://test-harness-{}.invalid", Uuid::new_v4());

    let response = announce_with_version(&http, &base, &caller_base_url, &net, "0.0.1").await;
    assert!(
        response.status().is_success(),
        "a below-floor version must not fail the request itself: {:?}",
        response.status()
    );

    let peers = list_peers(&http, &base).await;
    assert!(
        !peers.iter().any(|p| p["base_url"] == caller_base_url),
        "a below-floor-version peer must never be added to the peer table"
    );

    // Re-admission: the same peer, now reporting a real version.
    announce(&http, &base, &caller_base_url, &net).await;
    let peers = list_peers(&http, &base).await;
    assert!(
        peers.iter().any(|p| p["base_url"] == caller_base_url),
        "the same peer must be admitted normally once it reports a supported version"
    );
}

/// Requires `AVALON_SECOND_NODE_SERVER_URL` — a second real `avalon-server`
/// process sharing this network's `network_id` (any Postgres is fine,
/// shared or separate, since the peer table is in-memory per process).
/// Skipped, not failed, when unset.
#[tokio::test]
#[ignore]
async fn two_nodes_see_each_other_via_announce() {
    let Some(second_base) = second_node_url() else {
        eprintln!("skipping: AVALON_SECOND_NODE_SERVER_URL not set — see this file's module doc");
        return;
    };
    let http = reqwest::Client::new();
    let first_base = server_url();
    let net = network_id();

    // Node A announces itself to node B directly (bypassing A's own
    // worker/timer, same "drive the mechanism directly" approach
    // `tests/mirror_watcher.rs` already uses).
    let response = announce(&http, &second_base, &first_base, &net).await;
    assert!(response.status().is_success(), "{:?}", response.status());

    // Node B announces itself to node A.
    let response = announce(&http, &first_base, &second_base, &net).await;
    assert!(response.status().is_success(), "{:?}", response.status());

    let peers_of_b = list_peers(&http, &second_base).await;
    assert!(
        peers_of_b.iter().any(|p| p["base_url"] == first_base),
        "node B must know about node A after A announced to it"
    );

    let peers_of_a = list_peers(&http, &first_base).await;
    assert!(
        peers_of_a.iter().any(|p| p["base_url"] == second_base),
        "node A must know about node B after B announced to it"
    );
}
