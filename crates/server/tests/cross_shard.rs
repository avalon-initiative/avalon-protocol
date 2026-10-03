//! Issue #529's own stated acceptance test: two independent nodes given
//! the same gossiped shard STHs produce byte-identical cross-shard
//! roots. Exercised against two real, separately-running `avalon-server`
//! processes, each independently computing `GET /ledger/cross-shard-root`
//! over `AVALON_KNOWN_SHARDS`. Gated `--ignored`/live.
//!
//! Both aggregator nodes are configured with an identical
//! `AVALON_KNOWN_SHARDS` map naming two shard aliases that both resolve
//! to the same real, already-committed-history authority node (the only
//! node in this sandbox with real ledger content) — a non-trivial
//! (two-leaf) tree, deliberately not the one-shard degenerate case, so
//! this actually exercises canonical ordering and aggregation, not just a
//! pass-through:
//!
//! ```text
//! # authority — the usual `make start` config, real committed history
//!
//! # aggregator A
//! AVALON_SERVER_ADDR=127.0.0.1:18092
//! AVALON_KNOWN_SHARDS=shard_x=<authority-url>,shard_y=<authority-url>
//! AVALON_SHARD_VERIFY_KEYS=shard_x=<authority-verify-key-hex>,shard_y=<authority-verify-key-hex>
//!
//! # aggregator B — same config, different port
//! AVALON_SERVER_ADDR=127.0.0.1:18093
//! AVALON_KNOWN_SHARDS=shard_x=<authority-url>,shard_y=<authority-url>
//! AVALON_SHARD_VERIFY_KEYS=shard_x=<authority-verify-key-hex>,shard_y=<authority-verify-key-hex>
//!
//! AVALON_AGGREGATOR_A_URL=http://127.0.0.1:18092 \
//!   AVALON_AGGREGATOR_B_URL=http://127.0.0.1:18093 \
//!   cargo test -p avalon-server --test cross_shard -- --ignored
//! ```

fn require_aggregator_url(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| {
        panic!(
            "{var} not set — this test needs two independently-running avalon-server \
             processes, both configured with the identical AVALON_KNOWN_SHARDS/ \
             AVALON_SHARD_VERIFY_KEYS (see this file's module doc comment)"
        )
    })
}

#[tokio::test]
#[ignore]
async fn two_independent_nodes_given_the_same_shards_produce_identical_roots() {
    let http = reqwest::Client::new();
    let aggregator_a = require_aggregator_url("AVALON_AGGREGATOR_A_URL");
    let aggregator_b = require_aggregator_url("AVALON_AGGREGATOR_B_URL");

    let response_a: serde_json::Value = http
        .get(format!("{aggregator_a}/ledger/cross-shard-root"))
        .send()
        .await
        .expect("request to aggregator A failed — is it running?")
        .json()
        .await
        .unwrap();
    let response_b: serde_json::Value = http
        .get(format!("{aggregator_b}/ledger/cross-shard-root"))
        .send()
        .await
        .expect("request to aggregator B failed — is it running?")
        .json()
        .await
        .unwrap();

    assert!(
        !response_a["partial"].as_bool().unwrap(),
        "aggregator A's root should not be partial — check AVALON_SHARD_VERIFY_KEYS matches \
         the authority's real verify key: {response_a:?}"
    );
    assert!(
        !response_b["partial"].as_bool().unwrap(),
        "aggregator B's root should not be partial: {response_b:?}"
    );
    assert_eq!(
        response_a["shard_count"], 2,
        "expected the two-shard-alias fixture, got: {response_a:?}"
    );

    assert_eq!(
        response_a["root_hash"], response_b["root_hash"],
        "two independent nodes given the identical known-shard set must compute the byte-\
         identical cross-shard root — got A: {:?}, B: {:?}",
        response_a["root_hash"], response_b["root_hash"]
    );
}

/// A node with no `AVALON_KNOWN_SHARDS` configured falls back to the
/// one-shard degenerate case using its own local STH — no network calls,
/// still a valid (single-leaf) cross-shard root.
#[tokio::test]
#[ignore]
async fn an_unconfigured_node_falls_back_to_its_own_local_sth_as_the_one_shard() {
    let http = reqwest::Client::new();
    let base =
        std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());

    let response: serde_json::Value = http
        .get(format!("{base}/ledger/cross-shard-root"))
        .send()
        .await
        .expect("request failed — is `make start` running?")
        .json()
        .await
        .unwrap();

    assert_eq!(response["shard_count"], 1);
    assert_eq!(response["shards"][0]["shard_id"], "core");
    assert!(!response["partial"].as_bool().unwrap());
}

/// Two aggregators over the same shards agree on an owner's family head, its
/// inclusion proof verifies, and bad owners and members are refused.
/// `AVALON_FAMILY_OWNER` names a family the aggregators know (default
/// `game:agg-second`, as `scripts/live-tests.sh` configures).
#[tokio::test]
#[ignore]
async fn two_independent_nodes_agree_on_a_shard_family_head_and_its_proof() {
    let http = reqwest::Client::new();
    let a = require_aggregator_url("AVALON_AGGREGATOR_A_URL");
    let b = require_aggregator_url("AVALON_AGGREGATOR_B_URL");
    let owner =
        std::env::var("AVALON_FAMILY_OWNER").unwrap_or_else(|_| "game:agg-second".to_string());
    let get = |base: String, query: String| {
        let http = http.clone();
        async move {
            http.get(format!("{base}/ledger/shard-family?{query}"))
                .send()
                .await
                .expect("request failed")
        }
    };

    let resp_a: serde_json::Value = get(a.clone(), format!("owner={owner}"))
        .await
        .json()
        .await
        .unwrap();
    let resp_b: serde_json::Value = get(b, format!("owner={owner}")).await.json().await.unwrap();
    assert_eq!(resp_a["root_hash"], resp_b["root_hash"], "{resp_a:?}");
    assert_eq!(resp_a["shard_count"], 1, "{resp_a:?}");
    assert!(!resp_a["partial"].as_bool().unwrap(), "{resp_a:?}");

    let cross: serde_json::Value = http
        .get(format!("{a}/ledger/cross-shard-root"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(resp_a["root_hash"], cross["root_hash"]);

    let member = resp_a["members"][0]["shard_id"]
        .as_str()
        .unwrap()
        .to_string();
    let with_proof: serde_json::Value = get(a.clone(), format!("owner={owner}&member={member}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(with_proof["proof"]["shard_id"], member.as_str());
    assert_eq!(with_proof["proof"]["tree_size"], 1);

    assert_eq!(get(a.clone(), "owner=core".into()).await.status(), 400);
    assert_eq!(get(a.clone(), "owner=game:x/2".into()).await.status(), 400);
    assert_eq!(get(a.clone(), String::new()).await.status(), 400);
    assert_eq!(
        get(a, format!("owner={owner}&member=core")).await.status(),
        404
    );
}

/// `GET /integrations/{slug}/shards` lists the registered owner's verified
/// sibling with its head on the node hosting it, and is a 404 for an
/// integrator the node has no registration for.
#[tokio::test]
#[ignore]
async fn integrator_shards_lists_the_owners_verified_sibling() {
    let http = reqwest::Client::new();
    let second = require_aggregator_url("AVALON_SECOND_SHARD_URL");
    let a = require_aggregator_url("AVALON_AGGREGATOR_A_URL");

    let resp: serde_json::Value = http
        .get(format!("{second}/integrations/agg-second/shards"))
        .send()
        .await
        .expect("request failed")
        .json()
        .await
        .unwrap();
    assert_eq!(resp["owner"], "game:agg-second", "{resp:?}");
    assert_eq!(resp["shards"][0]["shard_id"], "game:agg-second", "{resp:?}");
    assert!(resp["shards"][0]["tree_size"].as_i64().unwrap() >= 1);
    assert_eq!(resp["shards"].as_array().unwrap().len(), 1, "{resp:?}");

    // Registered only on the second node, so the aggregator has no such integrator.
    let status = http
        .get(format!("{a}/integrations/agg-second/shards"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 404);
}
