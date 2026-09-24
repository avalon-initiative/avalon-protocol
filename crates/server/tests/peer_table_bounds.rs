//! Peer table bounds and announce admission against real `avalon-server`
//! processes. Gated `--ignored`; `scripts/live-tests.sh peer-table-bounds`
//! starts the nodes.
//!
//! Env: `PTB_CAP_URL` (private peers allowed, table capped at 5, no
//! reachability check, `PTB_REAL_URL` is its bootstrap peer and announces to
//! it), `PTB_STRICT_URL` (private peers refused), `PTB_LIMITED_URL` (3 new
//! URLs per source per minute), `PTB_REACH_URL` (reachability check on,
//! private peers allowed) and `PTB_OTHER_NET_URL` (a live node on another
//! network).

use serde_json::{json, Value};

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} not set; run via scripts/live-tests.sh"))
}

async fn status(node: &str) -> Value {
    reqwest::get(format!("{node}/nodes/status"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn announce_as(
    node: &str,
    base_url: &str,
    network_id: &Value,
    version: &Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{node}/nodes/announce"))
        .json(&json!({
            "base_url": base_url,
            "roles": ["combined"],
            "protocol_version": version,
            "network_id": network_id,
            "coordinate": {"vector": [0.0, 0.0, 0.0], "height": 0.01, "error": 1.0},
        }))
        .send()
        .await
        .unwrap()
}

async fn announce(node: &str, base_url: &str) -> reqwest::Response {
    let s = status(node).await;
    announce_as(node, base_url, &s["network_id"], &s["protocol_version"]).await
}

async fn peer_urls(node: &str) -> Vec<String> {
    let peers: Vec<Value> = reqwest::get(format!("{node}/nodes/peers"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    peers
        .iter()
        .map(|p| p["base_url"].as_str().unwrap().to_string())
        .collect()
}

async fn code(r: reqwest::Response) -> String {
    r.json::<Value>().await.unwrap()["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
#[ignore]
async fn flooding_fabricated_addresses_leaves_the_table_at_its_cap_and_real_peers_alive() {
    let (cap, real) = (var("PTB_CAP_URL"), var("PTB_REAL_URL"));
    for _ in 0..60 {
        if peer_urls(&cap).await.contains(&real) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(
        peer_urls(&cap).await.contains(&real),
        "real peer never announced"
    );

    for i in 0..40 {
        let r = announce(&cap, &format!("http://127.0.0.1:{}", 30000 + i)).await;
        assert!(r.status().is_success(), "announce {i}: {}", r.status());
    }
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let urls = peer_urls(&cap).await;
    assert_eq!(urls.len(), 5, "{urls:?}");
    assert!(
        urls.contains(&real),
        "the active bootstrap peer must survive: {urls:?}"
    );
    let newest = "http://127.0.0.1:30039".to_string();
    assert!(
        urls.contains(&newest),
        "the newest announcer replaces the oldest: {urls:?}"
    );
}

#[tokio::test]
#[ignore]
async fn forbidden_malformed_and_unreachable_announces_get_clear_4xx_and_are_not_admitted() {
    let strict = var("PTB_STRICT_URL");
    let before = peer_urls(&strict).await;
    let long = format!("http://8.8.8.8/{}", "a".repeat(400));
    let cases: Vec<(String, u16, &str)> = vec![
        ("http://127.0.0.1:9".into(), 403, "base_url_not_allowed"),
        ("http://10.1.2.3".into(), 403, "base_url_not_allowed"),
        ("http://169.254.169.254".into(), 403, "base_url_not_allowed"),
        ("http://localhost:9".into(), 403, "base_url_not_allowed"),
        ("ftp://8.8.8.8".into(), 400, "invalid_base_url"),
        ("http://user:pw@8.8.8.8".into(), 400, "invalid_base_url"),
        ("http://8.8.8.8/?q=1".into(), 400, "invalid_base_url"),
        (long, 400, "base_url_too_long"),
        ("http://192.0.2.1:9".into(), 422, "peer_unreachable"),
    ];
    for (url, status, want) in cases {
        let r = announce(&strict, &url).await;
        assert_eq!(r.status(), status, "{url}");
        assert_eq!(code(r).await, want, "{url}");
    }
    assert_eq!(peer_urls(&strict).await, before);
}

#[tokio::test]
#[ignore]
async fn over_the_per_source_new_url_limit_gets_429_with_retry_after() {
    let limited = var("PTB_LIMITED_URL");
    for i in 0..3 {
        let r = announce(&limited, &format!("http://127.0.0.1:{}", 31000 + i)).await;
        assert!(r.status().is_success(), "{}", r.status());
    }
    let r = announce(&limited, "http://127.0.0.1:31099").await;
    assert_eq!(r.status(), 429);
    let retry: u64 = r.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry));
    assert_eq!(code(r).await, "rate_limited");
    assert!(!peer_urls(&limited)
        .await
        .contains(&"http://127.0.0.1:31099".to_string()));

    let again = announce(&limited, "http://127.0.0.1:31000").await;
    assert!(
        again.status().is_success(),
        "refreshing a known entry is not a new URL"
    );
}

#[tokio::test]
#[ignore]
async fn a_new_announcer_must_answer_status_on_the_same_network() {
    let (reach, real, other) = (
        var("PTB_REACH_URL"),
        var("PTB_REAL_URL"),
        var("PTB_OTHER_NET_URL"),
    );
    let r = announce(&reach, "http://127.0.0.1:1").await;
    assert_eq!(r.status(), 422);
    assert_eq!(code(r).await, "peer_unreachable");

    let r = announce(&reach, &other).await;
    assert_eq!(r.status(), 422);
    assert_eq!(code(r).await, "peer_network_mismatch");

    let r = announce(&reach, &real).await;
    assert!(r.status().is_success(), "{}", r.status());
    let urls = peer_urls(&reach).await;
    assert!(urls.contains(&real));
    assert!(!urls.contains(&other));
}
