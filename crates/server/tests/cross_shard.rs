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
