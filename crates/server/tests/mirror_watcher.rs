//! Exercises the mirror-watcher's read-side surface (issue #299) against a
//! real, running `avalon-server` and Postgres. Gated `--ignored` since it
//! needs live infra — see `make test-live` / `make start`, same convention
//! `tests/settlement.rs` (#211) already uses.
//!
//! This ticket's POC topology is "one canonical Settlement authority, one
//! mirror node watching it" (see the ticket body / issue #301), which
//! needs two real `avalon-server` processes to exercise end-to-end —
//! not available in this sandbox even with a live Postgres reachable. What
//! *is* exercisable against a single running server plus its own Postgres:
//!
//! - The new bulk entries endpoint (`GET /ledger/entries`) returns real
//!   content in `seq` order, respecting `since_seq`/`limit`.
//! - A full "mirror" backfill pass: fetch a real STH, fetch real entries,
//!   fetch a real inclusion proof per entry, and verify each one
//!   client-side — the exact sequence `avalon-server`'s own
//!   `mirror_watcher::backfill` runs, just driven from the test instead of
//!   the background worker, so it's directly assertable.
//! - Equivocation detection against **real** Postgres storage
//!   (`avalon_chain::mirror`'s `observed_sths`/`equivocation_findings`
//!   tables): this test fetches one real, genuinely-signed STH from the
//!   live server, records it as an observation from "peer-a", then
//!   fabricates a second observation from "peer-b" claiming the same
//!   `network_id`/`tree_size` with a different `root_hash` — a deliberately
//!   corrupted STH, the exact scenario the ticket's acceptance criteria
//!   calls for — and confirms it's caught and durably recorded.

use avalon_chain::mirror::{self, ObservedSth};
use avalon_chain::{merkle, sth};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
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

async fn register_throwaway_integrator(http: &reqwest::Client, base: &str) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    let slug = format!("test-mirror-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": "Mirror Watcher Test Integrator",
        "owner_name": "Test Studio",
        "requested_capabilities": [],
        "initial_key": {
            "algorithm": "ed25519",
            "public_key": base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                [0u8; 32],
            ),
        },
    });
    let response = http
        .post(format!("{base}/integrators"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    format!("game:{slug}:self:registered")
}

async fn wait_for_committed_seq(pool: &PgPool, issuer: &str) -> i64 {
    for _ in 0..15 {
        if let Some(row) = sqlx::query("SELECT seq FROM ledger_entries WHERE issuer = $1")
            .bind(issuer)
            .fetch_optional(pool)
            .await
            .expect("query failed")
        {
            return row.try_get("seq").expect("seq column");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("entry for issuer `{issuer}` never appeared in ledger_entries — is the outbox worker running?");
}

async fn wait_for_covering_sth(pool: &PgPool, at_least_rank: i64) -> i64 {
    for _ in 0..15 {
        if let Some(row) = sqlx::query(
            "SELECT tree_size FROM signed_tree_heads WHERE tree_size >= $1 ORDER BY tree_size ASC LIMIT 1",
        )
        .bind(at_least_rank)
        .fetch_optional(pool)
        .await
        .expect("query failed")
        {
            return row.try_get("tree_size").expect("tree_size column");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("no signed_tree_heads row ever covered rank {at_least_rank}");
}

async fn entry_rank(pool: &PgPool, seq: i64) -> i64 {
    sqlx::query("SELECT COUNT(*) AS c FROM ledger_entries WHERE seq <= $1")
        .bind(seq)
        .fetch_one(pool)
        .await
        .expect("query failed")
        .try_get("c")
        .expect("c column")
}

fn hash32(hex_str: &str) -> [u8; 32] {
    let bytes = hex::decode(hex_str).expect("should be valid hex");
    <[u8; 32]>::try_from(bytes).expect("should be exactly 32 bytes")
}

fn sth_from_json(v: &serde_json::Value) -> sth::SignedTreeHead {
    sth::SignedTreeHead {
        tree_size: v["tree_size"].as_i64().unwrap(),
        root_hash: v["root_hash"].as_str().unwrap().to_string(),
        network_id: v["network_id"].as_str().unwrap().to_string(),
        signing_key_id: v["signing_key_id"].as_str().unwrap().to_string(),
        signature: v["signature"].as_str().unwrap().to_string(),
        created_at: time::OffsetDateTime::parse(
            v["created_at"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap(),
    }
}

/// `GET /ledger/entries` returns real content, oldest first, and respects
/// `since_seq` — the bulk read a mirror needs to hold entry content, not
/// just verify STHs.
#[tokio::test]
#[ignore]
async fn bulk_entries_endpoint_returns_real_content_in_seq_order_since_a_cursor() {
    let base = server_url();
    let http = reqwest::Client::new();
    let pool = test_pool().await;

    let issuer = register_throwaway_integrator(&http, &base).await;
    let seq = wait_for_committed_seq(&pool, &issuer).await;

    // Ask for everything strictly after the entry just before this one —
    // the new entry must be present, and (since since_seq excludes it)
    // nothing with seq <= since_seq must be.
    let response: serde_json::Value = http
        .get(format!("{base}/ledger/entries"))
        .query(&[("since_seq", seq - 1), ("limit", 50)])
        .send()
        .await
        .expect("GET /ledger/entries failed — is `make start` running?")
        .json()
        .await
        .unwrap();

    let entries = response
        .as_array()
        .expect("response should be a JSON array");
    assert!(
        !entries.is_empty(),
        "expected at least the freshly-committed entry"
    );

    let found = entries.iter().find(|e| e["seq"].as_i64() == Some(seq));
    let entry = found.expect("freshly-committed entry should be present in the bulk response");
    assert_eq!(entry["issuer"].as_str().unwrap(), issuer);

    // Strictly ascending, and nothing at or below the cursor.
    let mut last_seq = i64::MIN;
    for e in entries {
        let this_seq = e["seq"].as_i64().unwrap();
        assert!(
            this_seq > seq - 1,
            "returned an entry at or before since_seq"
        );
        assert!(
            this_seq > last_seq,
            "entries were not strictly ascending by seq"
        );
        last_seq = this_seq;
    }
}

/// The ticket's own backfill bar, run manually against a single live
/// server: fetch a real STH, fetch real entries via the new bulk endpoint,
/// fetch a real inclusion proof for each, and verify every one
/// client-side — exactly what `avalon-server`'s `mirror_watcher::backfill`
/// does per tick, just assembled here so it's directly assertable rather
/// than only observable via its side effects.
#[tokio::test]
#[ignore]
async fn a_mirror_can_backfill_and_verify_real_entries_against_a_real_sth() {
    let base = server_url();
    let http = reqwest::Client::new();
    let pool = test_pool().await;

    let issuer = register_throwaway_integrator(&http, &base).await;
    let seq = wait_for_committed_seq(&pool, &issuer).await;
    let rank = entry_rank(&pool, seq).await;
    let tree_size = wait_for_covering_sth(&pool, rank).await;

    let sth_json: serde_json::Value = http
        .get(format!("{base}/ledger/sth/{tree_size}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sth_struct = sth_from_json(&sth_json);

    let verify_key = sth::load_verify_key_from_env()
        .expect("AVALON_SETTLEMENT_VERIFY_KEY must be set to run this test");
    assert!(sth::verify_tree_head(&verify_key, &sth_struct));

    // Bulk-fetch everything up to tree_size, exactly as a mirror
    // backfilling from scratch would: paginated with a since_seq cursor,
    // not a single request — this dev ledger accumulates real history
    // across every test run, so it can already exceed one page's cap
    // (`MAX_ENTRIES_LIMIT`) well before `tree_size` is reached.
    let mut entries: Vec<serde_json::Value> = Vec::new();
    loop {
        let since_seq = entries
            .last()
            .and_then(|e: &serde_json::Value| e["seq"].as_i64())
            .unwrap_or(0);
        let page: Vec<serde_json::Value> = http
            .get(format!("{base}/ledger/entries"))
            .query(&[("since_seq", since_seq), ("limit", 1000)])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        entries.extend(page);
        if entries.len() as i64 >= tree_size {
            break;
        }
    }

    let root = hash32(&sth_struct.root_hash);
    let mut verified_count = 0usize;
    for entry in entries.iter().take(tree_size as usize) {
        let entry_seq = entry["seq"].as_i64().unwrap();
        let proof_json: serde_json::Value = http
            .get(format!("{base}/ledger/proof/inclusion"))
            .query(&[("seq", entry_seq), ("tree_size", tree_size)])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        assert_eq!(proof_json["root_hash"], sth_json["root_hash"]);
        assert_eq!(
            proof_json["leaf_hash"].as_str().unwrap(),
            entry["entry_hash"].as_str().unwrap(),
            "bulk-endpoint entry_hash disagreed with the peer's own inclusion-proof leaf_hash for seq={entry_seq}"
        );

        let leaf_bytes = hex::decode(entry["entry_hash"].as_str().unwrap()).unwrap();
        let proof: Vec<[u8; 32]> = proof_json["proof"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| hash32(h.as_str().unwrap()))
            .collect();

        assert!(
            merkle::verify_inclusion_proof(&leaf_bytes, verified_count, tree_size as usize, &proof, &root),
            "inclusion proof for seq={entry_seq} (leaf_index={verified_count}) failed independent verification"
        );
        verified_count += 1;
    }

    assert!(
        verified_count as i64 >= rank,
        "expected to have verified at least the freshly-committed entry's own rank ({rank}), verified {verified_count}"
    );
}

/// The ticket's other acceptance-criteria bar: a deliberately corrupted
/// second STH (same `network_id`/`tree_size`, different `root_hash`) gets
/// caught. Drives `avalon_chain::mirror`'s real storage/detection against
/// live Postgres — the same functions `avalon-server`'s mirror-watcher
/// background task calls — fabricating the "second peer" input rather than
/// standing up an actual second corrupted server, since equivocation is a
/// property of what gets *observed and stored*, not of the HTTP transport.
#[tokio::test]
#[ignore]
async fn a_deliberately_corrupted_sth_is_detected_as_equivocation() {
    let base = server_url();
    let http = reqwest::Client::new();
    let pool = test_pool().await;

    let sth_json: serde_json::Value = http
        .get(format!("{base}/ledger/sth/latest"))
        .send()
        .await
        .expect("GET /ledger/sth/latest failed — is `make start` running?")
        .json()
        .await
        .unwrap();
    let real_sth = sth_from_json(&sth_json);

    let source_a = format!("peer-a-{}", Uuid::new_v4());
    let source_b = format!("peer-b-{}", Uuid::new_v4());

    let observation_a =
        ObservedSth::from_sth(&source_a, &real_sth, time::OffsetDateTime::now_utc());
    let is_new_a = mirror::insert_observation(&pool, &observation_a)
        .await
        .expect("insert_observation failed");
    assert!(is_new_a);

    // The corrupted observation: same network_id/tree_size as the real
    // STH, a root_hash that does not match it, from a different source —
    // exactly the "same operator claiming two different trees at the same
    // size" scenario #40 defines as equivocation.
    let mut corrupted = real_sth.clone();
    corrupted.root_hash = "ff".repeat(32);
    let observation_b =
        ObservedSth::from_sth(&source_b, &corrupted, time::OffsetDateTime::now_utc());
    let is_new_b = mirror::insert_observation(&pool, &observation_b)
        .await
        .expect("insert_observation failed");
    assert!(is_new_b);

    let existing = mirror::observations_at(&pool, &real_sth.network_id, real_sth.tree_size)
        .await
        .expect("observations_at failed");
    let findings = mirror::detect_equivocation(&existing, &observation_b);
    assert_eq!(
        findings.len(),
        1,
        "expected exactly one disagreeing observation (peer-a)"
    );
    assert_eq!(findings[0].source_a, source_a);
    assert_eq!(findings[0].source_b, source_b);

    for finding in &findings {
        mirror::record_equivocation(&pool, finding)
            .await
            .expect("record_equivocation failed");
    }

    let recorded = mirror::list_equivocations(&pool, &real_sth.network_id)
        .await
        .expect("list_equivocations failed");
    assert!(
        recorded
            .iter()
            .any(|f| f.source_a == source_a && f.source_b == source_b),
        "equivocation finding was not durably recorded in equivocation_findings"
    );

    // Sanity: two consistent observations of the *same* correct STH from a
    // third source must never be flagged.
    let source_c = format!("peer-c-{}", Uuid::new_v4());
    let observation_c =
        ObservedSth::from_sth(&source_c, &real_sth, time::OffsetDateTime::now_utc());
    mirror::insert_observation(&pool, &observation_c)
        .await
        .expect("insert_observation failed");
    let existing_after_c = mirror::observations_at(&pool, &real_sth.network_id, real_sth.tree_size)
        .await
        .expect("observations_at failed");
    let findings_against_a = mirror::detect_equivocation(
        &existing_after_c
            .iter()
            .filter(|o| o.source_url == source_a)
            .cloned()
            .collect::<Vec<_>>(),
        &observation_c,
    );
    assert!(
        findings_against_a.is_empty(),
        "two consistent observations of the same real STH were incorrectly flagged as equivocation"
    );
}
