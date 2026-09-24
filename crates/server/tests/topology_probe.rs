//! `POST /nodes/probe` against real, running `avalon-server` processes. Gated
//! `--ignored`; `scripts/live-tests.sh topology` starts the nodes.
//!
//! Env: `TOPOLOGY_A_URL` (allows private peers, knows B and STRICT),
//! `TOPOLOGY_B_URL`, `TOPOLOGY_STRICT_URL` (does not allow private peers,
//! knows A), `TOPOLOGY_LIMITED_URL` (probe limit of 3 per minute).

use serde_json::{json, Value};

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} not set; run via scripts/live-tests.sh"))
}

async fn probe(node: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{node}/nodes/probe"))
        .json(&body)
        .send()
        .await
        .expect("POST /nodes/probe failed")
}

async fn wait_for_peer(node: &str, peer: &str) {
    let http = reqwest::Client::new();
    for _ in 0..60 {
        let peers: Vec<Value> = http
            .get(format!("{node}/nodes/peers"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if peers.iter().any(|p| p["base_url"] == peer) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("{node} never learned {peer}");
}

#[tokio::test]
#[ignore]
async fn probe_returns_timings_for_a_known_peer() {
    let (a, b) = (var("TOPOLOGY_A_URL"), var("TOPOLOGY_B_URL"));
    wait_for_peer(&a, &b).await;
    let r = probe(&a, json!({ "target": b, "samples": 3 })).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["ok"], true, "{body}");
    let samples = body["samples_ms"].as_array().unwrap();
    assert_eq!(samples.len(), 3);
    assert!(samples.iter().all(|v| v.as_f64().unwrap() > 0.0));
    assert!(body["min_ms"].as_f64().unwrap() <= body["median_ms"].as_f64().unwrap());
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert!(
        keys.iter().all(
            |k| ["target", "ok", "samples_ms", "min_ms", "median_ms", "error"]
                .contains(&k.as_str())
        ),
        "only timing fields may be returned: {keys:?}"
    );
}

#[tokio::test]
#[ignore]
async fn default_is_one_sample_and_more_than_three_is_rejected() {
    let (a, b) = (var("TOPOLOGY_A_URL"), var("TOPOLOGY_B_URL"));
    wait_for_peer(&a, &b).await;
    let body: Value = probe(&a, json!({ "target": b }))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["samples_ms"].as_array().unwrap().len(), 1);
    let r = probe(&a, json!({ "target": b, "samples": 4 })).await;
    assert_eq!(r.status(), 400);
    assert_eq!(r.json::<Value>().await.unwrap()["code"], "invalid_samples");
}

#[tokio::test]
#[ignore]
async fn unknown_target_is_rejected_distinctly() {
    let a = var("TOPOLOGY_A_URL");
    let r = probe(&a, json!({ "target": "http://127.0.0.1:1" })).await;
    assert_eq!(r.status(), 404);
    assert_eq!(r.json::<Value>().await.unwrap()["code"], "unknown_target");
}

#[tokio::test]
#[ignore]
async fn private_target_is_refused_without_the_allow_flag() {
    let (strict, a) = (var("TOPOLOGY_STRICT_URL"), var("TOPOLOGY_A_URL"));
    wait_for_peer(&strict, &a).await;
    let r = probe(&strict, json!({ "target": a })).await;
    assert_eq!(r.status(), 403);
    assert_eq!(
        r.json::<Value>().await.unwrap()["code"],
        "target_not_allowed"
    );
}

#[tokio::test]
#[ignore]
async fn link_local_metadata_address_is_refused_even_when_private_peers_are_allowed() {
    let a = var("TOPOLOGY_A_URL");
    let http = reqwest::Client::new();
    let status: Value = http
        .get(format!("{a}/nodes/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for target in ["http://169.254.169.254", "http://[fe80::1]:8080"] {
        let announce = http
            .post(format!("{a}/nodes/announce"))
            .json(&json!({
                "base_url": target,
                "roles": ["combined"],
                "protocol_version": status["protocol_version"],
                "network_id": status["network_id"],
            }))
            .send()
            .await
            .unwrap();
        assert!(announce.status().is_success());
        let r = probe(&a, json!({ "target": target })).await;
        assert_eq!(r.status(), 403, "{target}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["code"],
            "target_not_allowed"
        );
    }
}

#[tokio::test]
#[ignore]
async fn over_limit_gets_429_with_retry_after() {
    let limited = var("TOPOLOGY_LIMITED_URL");
    for _ in 0..3 {
        let r = probe(&limited, json!({ "target": "http://127.0.0.1:1" })).await;
        assert_eq!(r.status(), 404);
    }
    let r = probe(&limited, json!({ "target": "http://127.0.0.1:1" })).await;
    assert_eq!(r.status(), 429);
    let retry: u64 = r
        .headers()
        .get("retry-after")
        .expect("Retry-After header")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry));
    assert_eq!(r.json::<Value>().await.unwrap()["code"], "rate_limited");
}
