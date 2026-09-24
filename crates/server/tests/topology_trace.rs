//! `POST /nodes/trace` across real, running `avalon-server` processes wired in
//! a line and a ring. Gated `--ignored`; `scripts/live-tests.sh topology-trace`
//! starts the nodes and constrains each node's active set with bootstrap peers
//! and `AVALON_NODE_MAX_PEERS`.
//!
//! Env (comma-separated loopback ports, in wiring order):
//! - `TRACE_LINE`: line L0-L1-L2-L3, ordered so each node is strictly closer
//!   (overlay XOR distance) to L3 than the one before it
//! - `TRACE_RING`: ring R0-R1-R2-R3-R0
//! - `TRACE_EDGE`: node whose only neighbors are `TRACE_HOLE` (accepts
//!   connections, never answers) and `TRACE_CLOSED` (nothing listening)
//! - `TRACE_STRICT`: node that refuses private addresses, neighbor L0
//! - `TRACE_CAP`: one trace in flight at a time, neighbor `TRACE_HOLE`
//! - `TRACE_LIMITED`: 3 traces per minute per client

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use avalon_server::overlay_routing::{canonical_base_url, next_hop, NextHop, OverlayNode};
use serde_json::{json, Value};

fn ports(name: &str) -> Vec<u16> {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} not set; run via scripts/live-tests.sh"))
        .split(',')
        .map(|p| p.parse().unwrap())
        .collect()
}

fn port(name: &str) -> u16 {
    ports(name)[0]
}

fn url(p: u16) -> String {
    format!("http://127.0.0.1:{p}")
}

async fn post_trace(node: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{node}/nodes/trace"))
        .json(&body)
        .send()
        .await
        .expect("POST /nodes/trace failed")
}

async fn trace(node: &str, body: Value) -> Value {
    let r = post_trace(node, body).await;
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

fn hop_urls(t: &Value) -> Vec<String> {
    t["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["base_url"].as_str().unwrap().to_string())
        .collect()
}

async fn network_id(node: &str) -> String {
    let s: Value = reqwest::get(format!("{node}/nodes/status"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    s["network_id"].as_str().unwrap().to_string()
}

struct Expected {
    path: Vec<String>,
    reached: bool,
    reason: Option<&'static str>,
}

/// The path the overlay rule predicts, given each node's active neighbors.
fn simulate(
    net: &str,
    adjacency: &HashMap<String, Vec<String>>,
    from: &str,
    target: &str,
    mut ttl: u8,
) -> Expected {
    let node = |u: &str| OverlayNode {
        base_url: u.to_string(),
        network_id: net.to_string(),
        libp2p_peer_id: None,
    };
    let target_node = node(target);
    let mut visited = HashSet::new();
    let mut path = vec![];
    let mut at = from.to_string();
    loop {
        path.push(at.clone());
        if canonical_base_url(&at) == canonical_base_url(target) {
            return Expected {
                path,
                reached: true,
                reason: None,
            };
        }
        if ttl == 0 {
            return Expected {
                path,
                reached: false,
                reason: Some("ttl"),
            };
        }
        visited.insert(canonical_base_url(&at));
        let neighbors: Vec<OverlayNode> = adjacency[&at].iter().map(|u| node(u)).collect();
        match next_hop(&node(&at), &target_node, &neighbors, &visited) {
            NextHop::Direct(n) | NextHop::Forward(n) => at = n.base_url,
            NextHop::NoRoute(_) => {
                return Expected {
                    path,
                    reached: false,
                    reason: Some("no_route"),
                }
            }
        }
        ttl -= 1;
    }
}

fn line_adjacency(line: &[u16]) -> HashMap<String, Vec<String>> {
    (0..line.len())
        .map(|i| {
            let mut n = vec![];
            if i > 0 {
                n.push(url(line[i - 1]));
            }
            if i + 1 < line.len() {
                n.push(url(line[i + 1]));
            }
            (url(line[i]), n)
        })
        .collect()
}

fn ring_adjacency(ring: &[u16]) -> HashMap<String, Vec<String>> {
    let n = ring.len();
    (0..n)
        .map(|i| {
            (
                url(ring[i]),
                vec![url(ring[(i + n - 1) % n]), url(ring[(i + 1) % n])],
            )
        })
        .collect()
}

fn assert_matches(t: &Value, e: &Expected) {
    assert_eq!(hop_urls(t), e.path, "{t}");
    assert_eq!(t["reached"], e.reached, "{t}");
    assert_eq!(t["stopped_reason"].as_str(), e.reason, "{t}");
}

#[tokio::test]
#[ignore]
async fn trace_across_a_line_returns_ordered_hops_with_plausible_timings() {
    let line = ports("TRACE_LINE");
    let (first, last) = (url(line[0]), url(line[3]));
    let t = trace(&first, json!({ "target": last })).await;

    assert_eq!(t["reached"], true, "{t}");
    assert!(t["stopped_reason"].is_null());
    let expected: Vec<String> = line.iter().map(|p| url(*p)).collect();
    assert_eq!(hop_urls(&t), expected);

    let hops = t["hops"].as_array().unwrap();
    let total = t["total_ms"].as_f64().unwrap();
    assert!(total > 0.0 && total < 5000.0, "total_ms {total}");
    for (i, h) in hops.iter().enumerate() {
        assert_eq!(h["index"], i);
        assert!(!h["protocol_version"].as_str().unwrap().is_empty());
        assert_eq!(h["roles"], json!(["combined"]));
        let processing = h["processing_ms"].as_f64().unwrap();
        assert!((0.0..=total).contains(&processing), "hop {i}: {processing}");
        if i + 1 < hops.len() {
            let leg = h["to_next_ms"].as_f64().unwrap();
            assert!((0.0..=total).contains(&leg), "hop {i} leg {leg}");
        } else {
            assert!(h["to_next_ms"].is_null());
        }
    }
    assert_eq!(t["trace_id"].as_str().unwrap().len(), 36);
}

#[tokio::test]
#[ignore]
async fn trace_reuses_a_caller_supplied_trace_id() {
    let line = ports("TRACE_LINE");
    let id = "8f14e45f-ceea-4f67-9c5d-000000000001";
    let t = trace(
        &url(line[0]),
        json!({ "target": url(line[3]), "trace_id": id }),
    )
    .await;
    assert_eq!(t["trace_id"], id);
}

#[tokio::test]
#[ignore]
async fn ttl_stops_the_trace_and_reports_it() {
    let line = ports("TRACE_LINE");
    let t = trace(&url(line[0]), json!({ "target": url(line[3]), "ttl": 2 })).await;
    assert_eq!(t["reached"], false);
    assert_eq!(t["stopped_reason"], "ttl");
    let expected: Vec<String> = line[..3].iter().map(|p| url(*p)).collect();
    assert_eq!(hop_urls(&t), expected);
}

#[tokio::test]
#[ignore]
async fn a_target_with_no_route_reports_why_it_stopped() {
    let line = ports("TRACE_LINE");
    let net = network_id(&url(line[0])).await;
    let adjacency = line_adjacency(&line);
    let target = "http://127.0.0.1:1";
    let t = trace(&url(line[0]), json!({ "target": target })).await;
    assert_eq!(t["reached"], false);
    assert_eq!(t["stopped_reason"], "no_route");
    let detail = t["detail"].as_str().expect("no_route names its reason");
    assert!(["no_progress", "all_visited", "no_neighbors"].contains(&detail));
    assert_matches(&t, &simulate(&net, &adjacency, &url(line[0]), target, 12));
}

#[tokio::test]
#[ignore]
async fn every_line_pair_matches_the_overlay_rule() {
    let line = ports("TRACE_LINE");
    let net = network_id(&url(line[0])).await;
    let adjacency = line_adjacency(&line);
    for from in &line {
        for to in &line {
            let target = url(*to);
            let t = trace(&url(*from), json!({ "target": target })).await;
            assert_matches(&t, &simulate(&net, &adjacency, &url(*from), &target, 12));
        }
    }
}

#[tokio::test]
#[ignore]
async fn every_ring_pair_matches_the_overlay_rule_and_some_cross_several_hops() {
    let ring = ports("TRACE_RING");
    let net = network_id(&url(ring[0])).await;
    let adjacency = ring_adjacency(&ring);
    let mut longest = 0;
    for from in &ring {
        for to in &ring {
            let target = url(*to);
            let t = trace(&url(*from), json!({ "target": target })).await;
            let expected = simulate(&net, &adjacency, &url(*from), &target, 12);
            assert_matches(&t, &expected);
            longest = longest.max(hop_urls(&t).len());
        }
    }
    assert!(longest >= 3, "no ring trace crossed more than one link");
}

#[tokio::test]
#[ignore]
async fn a_visited_node_reports_a_loop() {
    let line = ports("TRACE_LINE");
    let me = url(line[1]);
    let t = trace(
        &me,
        json!({ "target": url(line[3]), "visited": [me.to_uppercase()], "ttl": 5 }),
    )
    .await;
    assert_eq!(t["reached"], false);
    assert_eq!(t["stopped_reason"], "loop");
    assert_eq!(hop_urls(&t), vec![me]);
}

#[tokio::test]
#[ignore]
async fn a_target_that_never_answers_ends_in_timeout() {
    let (edge, hole) = (url(port("TRACE_EDGE")), url(port("TRACE_HOLE")));
    let started = Instant::now();
    let t = trace(&edge, json!({ "target": hole, "budget_ms": 1500 })).await;
    let elapsed = started.elapsed();
    assert_eq!(t["reached"], false);
    assert_eq!(t["stopped_reason"], "timeout");
    assert_eq!(hop_urls(&t), vec![edge]);
    assert!(elapsed >= Duration::from_millis(1200), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
    assert!(t["hops"][0]["to_next_ms"].as_f64().unwrap() >= 1000.0);
}

#[tokio::test]
#[ignore]
async fn a_neighbor_that_refuses_connections_is_target_unreachable() {
    let (edge, closed) = (url(port("TRACE_EDGE")), url(port("TRACE_CLOSED")));
    let t = trace(&edge, json!({ "target": closed })).await;
    assert_eq!(t["reached"], false);
    assert_eq!(t["stopped_reason"], "target_unreachable");
    assert_eq!(t["detail"], "connect");
    assert_eq!(hop_urls(&t), vec![edge]);
}

#[tokio::test]
#[ignore]
async fn forwards_obey_the_outbound_policy() {
    let line = ports("TRACE_LINE");
    let strict = url(port("TRACE_STRICT"));
    let t = trace(&strict, json!({ "target": url(line[0]) })).await;
    assert_eq!(t["stopped_reason"], "target_unreachable");
    assert_eq!(t["detail"], "outbound_policy");
}

#[tokio::test]
#[ignore]
async fn untrusted_fields_are_clamped_and_bad_targets_rejected() {
    let line = ports("TRACE_LINE");
    let first = url(line[0]);
    let t = trace(
        &first,
        json!({ "target": url(line[3]), "ttl": 250, "budget_ms": 999999999u64 }),
    )
    .await;
    assert_eq!(t["reached"], true);
    for bad in [json!("ftp://x"), json!("http://u:p@x"), json!("nonsense")] {
        let r = post_trace(&first, json!({ "target": bad })).await;
        assert_eq!(r.status(), 400);
        assert_eq!(r.json::<Value>().await.unwrap()["code"], "invalid_target");
    }
    let own = trace(&first, json!({ "target": first })).await;
    assert_eq!(own["reached"], true);
    assert_eq!(hop_urls(&own), vec![first]);
}

#[tokio::test]
#[ignore]
async fn over_limit_gets_429_with_retry_after() {
    let limited = url(port("TRACE_LIMITED"));
    for _ in 0..3 {
        let r = post_trace(&limited, json!({ "target": limited })).await;
        assert_eq!(r.status(), 200);
    }
    let r = post_trace(&limited, json!({ "target": limited })).await;
    assert_eq!(r.status(), 429);
    let retry: u64 = r.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry));
    assert_eq!(r.json::<Value>().await.unwrap()["code"], "rate_limited");
}

#[tokio::test]
#[ignore]
async fn concurrent_traces_beyond_the_cap_get_429() {
    let (cap, hole) = (url(port("TRACE_CAP")), url(port("TRACE_HOLE")));
    let slow = tokio::spawn({
        let (cap, hole) = (cap.clone(), hole.clone());
        async move { trace(&cap, json!({ "target": hole, "budget_ms": 2000 })).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = post_trace(&cap, json!({ "target": cap })).await;
    assert_eq!(r.status(), 429);
    assert!(r.headers().get("retry-after").is_some());
    assert_eq!(
        r.json::<Value>().await.unwrap()["code"],
        "too_many_in_flight"
    );
    assert_eq!(slow.await.unwrap()["stopped_reason"], "timeout");
}
