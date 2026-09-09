//! Exercises the mirror-facing transparency-log endpoints (issue #211)
//! against a real, running `avalon-server` and Postgres. Gated `--ignored`
//! since it needs live infra — see `make test-live` / `make start`.
//!
//! Unlike most of this crate's other integration tests, these never
//! authenticate — `GET /ledger/sth/latest`, `GET /ledger/sth/{tree_size}`,
//! `GET /ledger/proof/consistency`, and `GET /ledger/proof/inclusion` are
//! all public reads by design (see `crates/server/src/settlement.rs`'s
//! module docs). What's exercised here is exactly the ticket's own
//! integration-test bar: fetch a real STH, fetch a real inclusion proof for
//! an entry that actually exists, and verify it **client-side**, with no
//! further server trust beyond the STH's Ed25519 signature —
//! `avalon_chain::merkle`/`avalon_chain::sth` are used directly here the
//! same way an independent mirror would use them, not re-trusting whatever
//! the server claims.
//!
//! A ledger entry only gets a `seq`/becomes provable once the outbox
//! worker (`crates/server/src/outbox.rs`, 3s poll) actually drains it into
//! a committed batch, so every test here polls briefly after triggering a
//! write rather than assuming it's already settled.

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

/// Registers a throwaway game via the real, unauthenticated `POST /games`
/// endpoint — the simplest real write in this codebase that goes through
/// the outbox into the ledger (`crates/server/src/games.rs::register_game`),
/// with no WebAuthn ceremony required. Returns the ledger `issuer` string
/// (`game:<slug>:self:registered`) this test polls `ledger_entries` for.
async fn register_throwaway_game(http: &reqwest::Client, base: &str) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    let slug = format!("test-settlement-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": "Settlement Proof Test Game",
        "developer": "Test Studio",
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
        .post(format!("{base}/games"))
        .json(&body)
        .send()
        .await
        .expect("register game failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    format!("game:{slug}:self:registered")
}

/// Polls `ledger_entries` for a row with this exact `issuer`, up to ~30s —
/// the outbox worker's own drain cadence — returning its `seq` once the
/// settlement worker has actually committed it.
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

/// Polls `signed_tree_heads` for the smallest `tree_size >= at_least_seq`,
/// up to ~30s — an STH is only produced once the *batch* containing
/// `at_least_seq` has closed, which can be a moment after the row itself
/// lands in `ledger_entries` (same transaction as the batch's own
/// `ledger_batches` row, committed right after the loop that inserts
/// entries — see `PostgresSettlementProvider::commit`).
async fn wait_for_covering_sth(pool: &PgPool, at_least_seq: i64) -> i64 {
    for _ in 0..15 {
        if let Some(row) = sqlx::query(
            "SELECT tree_size FROM signed_tree_heads WHERE tree_size >= $1 ORDER BY tree_size ASC LIMIT 1",
        )
        .bind(at_least_seq)
        .fetch_optional(pool)
        .await
        .expect("query failed")
        {
            return row.try_get("tree_size").expect("tree_size column");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("no signed_tree_heads row ever covered seq {at_least_seq}");
}

fn root_hash_bytes(hex_str: &str) -> [u8; 32] {
    let bytes = hex::decode(hex_str).expect("root_hash should be valid hex");
    <[u8; 32]>::try_from(bytes).expect("root_hash should be exactly 32 bytes")
}

#[tokio::test]
#[ignore]
async fn latest_sth_is_a_real_signed_tree_head() {
    let base = server_url();
    let http = reqwest::Client::new();

    let response = http
        .get(format!("{base}/ledger/sth/latest"))
        .send()
        .await
        .expect("GET /ledger/sth/latest failed — is `make start` running?");
    assert!(response.status().is_success(), "{:?}", response.status());
    let sth: serde_json::Value = response.json().await.unwrap();

    let tree_size = sth["tree_size"].as_i64().unwrap();
    assert!(tree_size >= 1, "expected at least one committed entry");
    assert!(sth["root_hash"].as_str().unwrap().len() == 64);
    assert!(sth["signature"].as_str().unwrap().len() == 128);
}

/// The ticket's own integration bar: fetch a real STH, fetch a real
/// inclusion proof for an entry that genuinely exists, and verify it
/// client-side with no further server trust beyond the STH's signature.
#[tokio::test]
#[ignore]
async fn inclusion_proof_for_a_real_entry_verifies_client_side_against_its_sth() {
    let base = server_url();
    let http = reqwest::Client::new();
    let pool = test_pool().await;

    let issuer = register_throwaway_game(&http, &base).await;
    let seq = wait_for_committed_seq(&pool, &issuer).await;
    let tree_size = wait_for_covering_sth(&pool, seq).await;

    // Fetch the STH covering this entry, over HTTP — exactly what a mirror
    // would do, not a DB read.
    let sth_response: serde_json::Value = http
        .get(format!("{base}/ledger/sth/{tree_size}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sth_struct = sth::SignedTreeHead {
        tree_size: sth_response["tree_size"].as_i64().unwrap(),
        root_hash: sth_response["root_hash"].as_str().unwrap().to_string(),
        network_id: sth_response["network_id"].as_str().unwrap().to_string(),
        signing_key_id: sth_response["signing_key_id"].as_str().unwrap().to_string(),
        signature: sth_response["signature"].as_str().unwrap().to_string(),
        created_at: time::OffsetDateTime::parse(
            sth_response["created_at"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap(),
    };

    // Verify the STH's signature with only the public key — the only trust
    // anchor a real mirror has (`AVALON_SETTLEMENT_VERIFY_KEY`, same env
    // var `avalon inspect-ledger` uses).
    let verify_key = sth::load_verify_key_from_env()
        .expect("AVALON_SETTLEMENT_VERIFY_KEY must be set to run this test");
    assert!(
        sth::verify_tree_head(&verify_key, &sth_struct),
        "STH signature failed to verify against AVALON_SETTLEMENT_VERIFY_KEY"
    );

    // Fetch the inclusion proof, over HTTP.
    let proof_response: serde_json::Value = http
        .get(format!("{base}/ledger/proof/inclusion"))
        .query(&[("seq", seq), ("tree_size", tree_size)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(proof_response["root_hash"], sth_response["root_hash"]);

    let leaf_bytes = hex::decode(proof_response["leaf_hash"].as_str().unwrap()).unwrap();
    let proof: Vec<[u8; 32]> = proof_response["proof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| root_hash_bytes(h.as_str().unwrap()))
        .collect();
    let root = root_hash_bytes(sth_struct.root_hash.as_str());

    // Client-side verification, no further server trust: only the leaf
    // bytes, its claimed position, the proof, and the already-signature-
    // verified root.
    assert!(
        merkle::verify_inclusion_proof(
            &leaf_bytes,
            (seq - 1) as usize,
            tree_size as usize,
            &proof,
            &root
        ),
        "inclusion proof for a real, freshly-committed entry failed independent verification"
    );
}

#[tokio::test]
#[ignore]
async fn consistency_proof_between_two_real_tree_sizes_verifies_client_side() {
    let base = server_url();
    let http = reqwest::Client::new();
    let pool = test_pool().await;

    // Two real, distinct committed tree sizes: whatever the ledger already
    // has, plus one more produced by this test's own write.
    let first_tree_size: i64 = sqlx::query(
        "SELECT tree_size FROM signed_tree_heads ORDER BY tree_size ASC LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    .unwrap()
    .map(|row| row.try_get("tree_size").unwrap())
    .expect("expected at least one existing signed_tree_heads row — run the inclusion-proof test first, or seed the ledger");

    let issuer = register_throwaway_game(&http, &base).await;
    let seq = wait_for_committed_seq(&pool, &issuer).await;
    let second_tree_size = wait_for_covering_sth(&pool, seq).await;
    assert!(second_tree_size >= first_tree_size);

    let first_sth: serde_json::Value = http
        .get(format!("{base}/ledger/sth/{first_tree_size}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_sth: serde_json::Value = http
        .get(format!("{base}/ledger/sth/{second_tree_size}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let proof_response: serde_json::Value = http
        .get(format!("{base}/ledger/proof/consistency"))
        .query(&[("first", first_tree_size), ("second", second_tree_size)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let proof: Vec<[u8; 32]> = proof_response["proof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| root_hash_bytes(h.as_str().unwrap()))
        .collect();
    let old_root = root_hash_bytes(first_sth["root_hash"].as_str().unwrap());
    let new_root = root_hash_bytes(second_sth["root_hash"].as_str().unwrap());

    assert!(
        merkle::verify_consistency_proof(
            first_tree_size as usize,
            second_tree_size as usize,
            &proof,
            &old_root,
            &new_root
        ),
        "consistency proof between two real committed tree sizes failed independent verification"
    );
}

/// Negative case the ticket calls out explicitly: a `tree_size`/`seq`
/// beyond what's actually been committed must be a clear error, never a
/// fabricated proof.
#[tokio::test]
#[ignore]
async fn inclusion_proof_for_an_uncommitted_tree_size_is_a_clear_error() {
    let base = server_url();
    let http = reqwest::Client::new();

    let response = http
        .get(format!("{base}/ledger/proof/inclusion"))
        .query(&[("seq", 1_i64), ("tree_size", i64::MAX / 2)])
        .send()
        .await
        .unwrap();

    assert!(
        !response.status().is_success(),
        "expected an error for a tree_size far beyond anything committed, got {:?}",
        response.status()
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body.get("error").is_some(),
        "expected a clear error body, got {body:?}"
    );
}

#[tokio::test]
#[ignore]
async fn sth_at_an_uncommitted_tree_size_is_not_found() {
    let base = server_url();
    let http = reqwest::Client::new();

    let response = http
        .get(format!("{base}/ledger/sth/{}", i64::MAX / 2))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
}
