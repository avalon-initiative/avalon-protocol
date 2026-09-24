//! `GET /nodes/topology` against three real `avalon-server` processes wired
//! by `scripts/live-tests.sh` (group `topology`). Gated `--ignored`.
//!
//! Node A (`AVALON_SERVER_URL`) bootstraps to node B and to an unreachable
//! address, with an active-set cap of two; node C is only reachable through
//! gossip, so A knows it without it being an active neighbor.

use std::time::{Duration, Instant};

use serde_json::Value;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set (see scripts/live-tests.sh)"))
}

async fn fetch(http: &reqwest::Client, base: &str, query: &str) -> (Value, Option<String>) {
    let response = http
        .get(format!("{base}/nodes/topology{query}"))
        .send()
        .await
        .expect("topology request failed");
    assert!(response.status().is_success(), "{}", response.status());
    let cache = response
        .headers()
        .get("cache-control")
        .map(|v| v.to_str().unwrap().to_string());
    (response.json().await.expect("topology json"), cache)
}

fn base_urls(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|n| n["base_url"].as_str().unwrap().to_string())
        .collect()
}

fn neighbor<'a>(topo: &'a Value, url: &str) -> Option<&'a Value> {
    topo["neighbors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["base_url"] == url)
}

#[tokio::test]
#[ignore]
async fn topology_separates_neighbors_from_known_and_reports_latency() {
    let a = env("AVALON_SERVER_URL");
    let b = env("AVALON_TOPOLOGY_NODE_B_URL");
    let c = env("AVALON_TOPOLOGY_NODE_C_URL");
    let dead = env("AVALON_TOPOLOGY_DEAD_PEER_URL");
    let http = reqwest::Client::new();

    let deadline = Instant::now() + Duration::from_secs(60);
    let topo = loop {
        let (topo, _) = fetch(&http, &a, "").await;
        let b_measured =
            neighbor(&topo, &b).is_some_and(|n| n["latency"]["samples"].as_u64() >= Some(1));
        let dead_lost = neighbor(&topo, &dead)
            .is_some_and(|n| n["latency"]["failed_recent"].as_u64() >= Some(1));
        if b_measured && dead_lost && base_urls(&topo["known"]).contains(&c) {
            break topo;
        }
        assert!(
            Instant::now() < deadline,
            "topology never converged: {topo}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    };

    assert_eq!(topo["self"]["base_url"], a.as_str());
    assert!(topo["generated_at"].is_string());

    let neighbors = base_urls(&topo["neighbors"]);
    assert_eq!(
        neighbors.len(),
        2,
        "active set is capped at two: {neighbors:?}"
    );
    assert!(
        !neighbors.contains(&c),
        "gossip-only peer must not be active"
    );
    let known = base_urls(&topo["known"]);
    assert!(known.contains(&c));
    assert!(known.iter().all(|k| !neighbors.contains(k)));

    for n in topo["neighbors"].as_array().unwrap() {
        assert_eq!(n["bootstrap"], true);
        let lat = &n["latency"];
        assert_eq!(lat["observed_by"], a.as_str());
        assert_eq!(lat["measurement"], "application_round_trip");
        if n["base_url"] == b.as_str() {
            assert!(lat["last_ms"].as_f64().unwrap() > 0.0);
            assert!(lat["ewma_ms"].as_f64().unwrap() > 0.0);
        } else {
            assert!(
                lat["last_ms"].is_null(),
                "an unreachable peer has no latency"
            );
            assert_eq!(lat["samples"], 0);
            assert!(lat["loss_ratio"].as_f64().unwrap() > 0.0);
        }
    }

    let mirrors = topo["mirrors"].as_array().unwrap();
    assert_eq!(mirrors.len(), 1);
    assert_eq!(mirrors[0]["shard_id"], "core");
    assert_eq!(mirrors[0]["source_url"], b.as_str());
    assert!(mirrors[0]["open_equivocations"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore]
async fn topology_limit_bounds_known_and_response_is_cacheable() {
    let a = env("AVALON_SERVER_URL");
    let c = env("AVALON_TOPOLOGY_NODE_C_URL");
    let http = reqwest::Client::new();

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (topo, _) = fetch(&http, &a, "").await;
        if base_urls(&topo["known"]).contains(&c) {
            break;
        }
        assert!(Instant::now() < deadline, "node C never became known");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let (topo, cache) = fetch(&http, &a, "?limit=0").await;
    assert!(topo["known"].as_array().unwrap().is_empty());
    assert!(topo["known_total"].as_u64().unwrap() >= 1);
    assert!(cache.unwrap().contains("max-age"));
}

#[tokio::test]
#[ignore]
async fn topology_publishes_finite_coordinates_for_self_and_neighbors() {
    use avalon_server::network_coordinates::{estimate_rtt_ms, Coordinate};

    let a = env("AVALON_SERVER_URL");
    let b = env("AVALON_TOPOLOGY_NODE_B_URL");
    let dead = env("AVALON_TOPOLOGY_DEAD_PEER_URL");
    let http = reqwest::Client::new();

    let deadline = Instant::now() + Duration::from_secs(60);
    let topo = loop {
        let (topo, _) = fetch(&http, &a, "").await;
        if neighbor(&topo, &b).is_some_and(|n| n["coordinate"].is_object()) {
            break topo;
        }
        assert!(Instant::now() < deadline, "no neighbor coordinate: {topo}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    };

    let own: Coordinate = serde_json::from_value(topo["self"]["coordinate"].clone()).unwrap();
    let theirs: Coordinate =
        serde_json::from_value(neighbor(&topo, &b).unwrap()["coordinate"].clone()).unwrap();
    assert!(own.is_valid() && theirs.is_valid(), "{own:?} {theirs:?}");
    let estimate = estimate_rtt_ms(&own, &theirs);
    assert!(estimate.is_finite() && estimate >= 0.0);

    let (b_topo, _) = fetch(&http, &b, "").await;
    let b_own: Coordinate = serde_json::from_value(b_topo["self"]["coordinate"].clone()).unwrap();
    assert!(b_own.is_valid());
    assert!(neighbor(&topo, &dead).is_some_and(|n| n["coordinate"].is_null()));
}
