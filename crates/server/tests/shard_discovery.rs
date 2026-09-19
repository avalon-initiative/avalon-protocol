//! Issue #599: automatic mainnet shard discovery, both layers. Gated
//! `--ignored`/live, following the same "drive the real mechanism against
//! real, separately-running processes" convention
//! `crates/server/tests/cross_shard.rs`/`crates/server/tests/nodes.rs`
//! already establish — no mocked HTTP anywhere in this file.
//!
//! **Layer 1 (peer-set growth past the bootstrap list)** needs a genuine
//! three-node chain: A bootstraps only from B, B bootstraps only from C —
//! neither A nor the test itself is ever told C's URL through
//! `AVALON_BOOTSTRAP_PEERS` or a direct announce call. The only way A ends
//! up actively exchanging with C is if A's own background worker promoted
//! C (learned via B's announce response) into its active announce set and
//! used it. This is externally observable: C's own `GET /nodes/peers`
//! eventually lists A, even though A→C was never a configured edge and
//! this test never manually announces A to C.
//!
//! Set `AVALON_NODE_A_URL`/`AVALON_NODE_B_URL`/`AVALON_NODE_C_URL` to run
//! this against a real chain topology (see this repo's
//! `docs/architecture/settlement.md`'s "Automatic shard discovery" section
//! for the exact three-machine setup this was live-verified against:
//! this sandbox, `avalon-peer`, `avalon-peer-two`). Skipped, not failed,
//! when unset — same pattern `tests/nodes.rs`'s
//! `two_nodes_see_each_other_via_announce` already uses for its own
//! second-node scenario.
//!
//! **Layer 2 (shard-existence gossip)** is exercised indirectly through
//! the same chain: as long as node A ends up actively exchanging with the
//! shard-authoritative node (directly or transitively), and A's own
//! Postgres has the shard's `issuer_keys` (`purpose = 'shard_settlement'`)
//! registration already on record (the normal #543 onboarding step, done
//! once via `POST /integrations/{slug}/keys` against A directly — this is
//! not something #599 needs to shortcut), `GET /ledger/cross-shard-root`
//! on A should include that shard with `partial: false`, with zero
//! `AVALON_KNOWN_SHARDS` configured on A.
//!
//! **Auto-mirroring** is checked the same way, but reading
//! `avalon_chain::mirror::mirrored_progress` against A's own database
//! directly, when `AVALON_SHARD_DISCOVERY_MIRROR_TEST_SHARD_ID` names the
//! shard to check and A is known to run with
//! `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true`.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

fn node_a_url() -> Option<String> {
    std::env::var("AVALON_NODE_A_URL").ok()
}

fn node_c_url() -> Option<String> {
    std::env::var("AVALON_NODE_C_URL").ok()
}

async fn get_peers(http: &reqwest::Client, base: &str) -> Vec<serde_json::Value> {
    http.get(format!("{base}/nodes/peers"))
        .send()
        .await
        .expect("GET /nodes/peers failed — is the node reachable?")
        .json()
        .await
        .expect("GET /nodes/peers response was not JSON")
}

/// Layer 1's own stated acceptance test: given a real three-node
/// bootstrap chain A→B→C (A never told about C directly), C eventually
/// lists A in its own peer table — proof A's active announce set grew
/// past its single configured bootstrap peer (B). Polls for up to two
/// minutes (a few announce-interval ticks on a fast test config) before
/// giving up, since this depends on each node's own background worker
/// timer, not something this test can force a tick of directly.
#[tokio::test]
#[ignore]
async fn a_node_bootstrapped_only_through_an_intermediate_peer_eventually_reaches_the_far_end() {
    let (Some(node_a), Some(node_c)) = (node_a_url(), node_c_url()) else {
        eprintln!(
            "skipping: AVALON_NODE_A_URL/AVALON_NODE_C_URL not set — see this file's module doc \
             for the three-node chain this needs"
        );
        return;
    };
    let http = reqwest::Client::new();

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(120);
    loop {
        let peers_of_c = get_peers(&http, &node_c).await;
        if peers_of_c.iter().any(|p| p["base_url"] == node_a) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "node C ({node_c}) never learned about node A ({node_a}) — Layer 1 peer-set \
                 growth did not propagate A past its bootstrap peer within the deadline. C's \
                 current peers: {peers_of_c:?}"
            );
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

/// Layer 2: a node with zero `AVALON_KNOWN_SHARDS` configured computes a
/// non-partial cross-shard root that includes a shard it only ever heard
/// about via peer-announce gossip. `AVALON_SHARD_DISCOVERY_SHARD_ID` names
/// the shard to look for in node A's aggregated response (the shard some
/// other node on the chain is authoritative for).
#[tokio::test]
#[ignore]
async fn a_node_discovers_and_verifies_a_shard_it_was_never_configured_with() {
    let Some(node_a) = node_a_url() else {
        eprintln!("skipping: AVALON_NODE_A_URL not set — see this file's module doc");
        return;
    };
    let Some(shard_id) = std::env::var("AVALON_SHARD_DISCOVERY_SHARD_ID").ok() else {
        eprintln!(
            "skipping: AVALON_SHARD_DISCOVERY_SHARD_ID not set — names the shard node A should \
             discover purely via gossip"
        );
        return;
    };
    let http = reqwest::Client::new();

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(180);
    loop {
        let response: serde_json::Value = http
            .get(format!("{node_a}/ledger/cross-shard-root"))
            .send()
            .await
            .expect("GET /ledger/cross-shard-root failed")
            .json()
            .await
            .expect("response was not JSON");

        let shards = response["shards"].as_array().cloned().unwrap_or_default();
        let found = shards.iter().any(|s| s["shard_id"] == shard_id);
        let missing = response["missing_shard_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        if found {
            assert!(
                !missing.iter().any(|m| m == &shard_id),
                "shard {shard_id} appears in both `shards` and `missing_shard_ids` — should be \
                 impossible: {response:?}"
            );
            return;
        }

        if tokio::time::Instant::now() >= deadline {
            panic!(
                "node A never discovered/verified shard {shard_id} via gossip within the \
                 deadline — full response: {response:?}"
            );
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
}

async fn live_test_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let database_url =
        std::env::var("AVALON_NODE_A_DATABASE_URL").expect("AVALON_NODE_A_DATABASE_URL must be set — node A's own database, to confirm auto-mirroring wrote real rows there");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to node A's Postgres — is it reachable?")
}

/// A node configured with `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true`
/// actually starts mirroring a shard it never had in
/// `AVALON_MIRROR_PEERS`/`AVALON_KNOWN_SHARDS` — checked by reading node
/// A's own `mirrored_entries`/mirror-progress state directly, the same
/// durable record `crate::mirror_watcher::backfill` writes for any
/// mirrored shard, discovered or statically configured.
#[tokio::test]
#[ignore]
async fn a_node_with_auto_mirror_enabled_starts_mirroring_a_discovered_shard() {
    let Some(network_id) = std::env::var("AVALON_NETWORK_ID").ok() else {
        eprintln!("skipping: AVALON_NETWORK_ID not set — see this file's module doc");
        return;
    };
    let Some(shard_url) = std::env::var("AVALON_SHARD_DISCOVERY_SHARD_URL").ok() else {
        eprintln!(
            "skipping: AVALON_SHARD_DISCOVERY_SHARD_URL not set — the discovered shard's own \
             base_url, which mirrored_entries.source_url is tagged with"
        );
        return;
    };
    if std::env::var("AVALON_NODE_A_DATABASE_URL").is_err() {
        eprintln!(
            "skipping: AVALON_NODE_A_DATABASE_URL not set — node A's own database is needed to \
             confirm mirrored rows landed there"
        );
        return;
    }
    let Some(shard_id) = std::env::var("AVALON_SHARD_DISCOVERY_SHARD_ID").ok() else {
        eprintln!(
            "skipping: AVALON_SHARD_DISCOVERY_SHARD_ID not set — the discovered shard's own \
             shard_id, needed to scope the mirrored_progress check (issue #604)"
        );
        return;
    };
    let pool = live_test_pool().await;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(180);
    loop {
        let progress = avalon_chain::mirror::mirrored_progress(
            &pool,
            &network_id,
            &shard_id,
            Some(&shard_url),
        )
        .await
        .expect("mirrored_progress query failed");
        if progress.verified_count > 0 {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "node A never auto-mirrored any entries from discovered shard peer {shard_url} \
                 within the deadline — is AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true on node A, \
                 and does it have a #543-registered shard_settlement key for this shard's \
                 integrator?"
            );
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
}

/// Regression test for a real bug this ticket's own live verification
/// caught (present since #529, not introduced by #599, but only actually
/// hit once a node commonly both authors its own shard *and* has other
/// shards known via config or gossip — exactly what #599 makes common):
/// `compute_for_this_node` used to be an either/or branch between "fetch
/// every externally-known shard" and "fall back to my own local shard,
/// only when nothing else is known" — the moment any other shard became
/// known, a node's own locally-authored shard silently dropped out of its
/// own `GET /ledger/cross-shard-root` response entirely, not even
/// appearing in `missing_shard_ids`. Fixed to always include the local
/// shard directly from `state.chain`, unioned with whatever else is known
/// — never gated on whether anything else happens to be known too.
///
/// `AVALON_NODE_A_OWN_SHARD_ID` names node A's own shard id (defaults to
/// `"core"`, matching `AVALON_OWN_SHARD_ID`'s own default). Reuses
/// `AVALON_SHARD_DISCOVERY_SHARD_ID` for the other, externally-known
/// shard this test also asserts is present alongside it.
#[tokio::test]
#[ignore]
async fn a_node_authoring_its_own_shard_keeps_it_once_another_shard_is_also_known() {
    let Some(node_a) = node_a_url() else {
        eprintln!("skipping: AVALON_NODE_A_URL not set — see this file's module doc");
        return;
    };
    let Some(other_shard_id) = std::env::var("AVALON_SHARD_DISCOVERY_SHARD_ID").ok() else {
        eprintln!(
            "skipping: AVALON_SHARD_DISCOVERY_SHARD_ID not set — names the other, externally-\
             known shard this test expects to see alongside node A's own"
        );
        return;
    };
    let own_shard_id =
        std::env::var("AVALON_NODE_A_OWN_SHARD_ID").unwrap_or_else(|_| "core".to_string());
    let http = reqwest::Client::new();

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(180);
    loop {
        let response: serde_json::Value = http
            .get(format!("{node_a}/ledger/cross-shard-root"))
            .send()
            .await
            .expect("GET /ledger/cross-shard-root failed")
            .json()
            .await
            .expect("response was not JSON");

        let shards = response["shards"].as_array().cloned().unwrap_or_default();
        let has_own = shards.iter().any(|s| s["shard_id"] == own_shard_id);
        let has_other = shards.iter().any(|s| s["shard_id"] == other_shard_id);

        if has_own && has_other {
            return;
        }

        if tokio::time::Instant::now() >= deadline {
            panic!(
                "node A's own shard ({own_shard_id}, present: {has_own}) and/or the other \
                 known shard ({other_shard_id}, present: {has_other}) missing from \
                 /ledger/cross-shard-root within the deadline — full response: {response:?}"
            );
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
}
