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

/// Registers a throwaway integrator via the real, unauthenticated `POST /integrations`
/// endpoint — the simplest real write in this codebase that goes through
/// the outbox into the ledger (`crates/server/src/integrations.rs::register_integrator`),
/// with no WebAuthn ceremony required. Returns the ledger `issuer` string
/// (`game:<slug>:self:registered`) this test polls `ledger_entries` for.
async fn register_throwaway_integrator(http: &reqwest::Client, base: &str) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    let slug = format!("test-settlement-{}", &suffix[..12]);
    let body = serde_json::json!({
        "slug": slug,
        "name": "Settlement Proof Test Integrator",
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
        .post(format!("{base}/integrations"))
        .json(&body)
        .send()
        .await
        .expect("register integrator failed — is `make start` running?");
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

/// This entry's rank among all committed entries, oldest first (1 =
/// oldest) — `seq` is a real row identifier, not a dense position, so it
/// can't be compared against `tree_size` (a leaf *count*) directly; `seq`
/// can and does have gaps (see `crates/chain/src/postgres.rs`'s module doc
/// comment). Same "count, don't subtract" approach the leaf-index
/// computation below already uses, just 1-based instead of 0-based.
async fn entry_rank(pool: &PgPool, seq: i64) -> i64 {
    sqlx::query("SELECT COUNT(*) AS c FROM ledger_entries WHERE seq <= $1")
        .bind(seq)
        .fetch_one(pool)
        .await
        .expect("query failed")
        .try_get("c")
        .expect("c column")
}

/// Polls `signed_tree_heads` for the smallest `tree_size >= at_least_rank`
/// (an entry's rank, from [`entry_rank`] — never a raw `seq`), up to ~30s —
/// an STH is only produced once the *batch* containing this entry has
/// closed, which can be a moment after the row itself lands in
/// `ledger_entries` (same transaction as the batch's own `ledger_batches`
/// row, committed right after the loop that inserts entries — see
/// `PostgresSettlementProvider::commit`).
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

    let issuer = register_throwaway_integrator(&http, &base).await;
    let seq = wait_for_committed_seq(&pool, &issuer).await;
    let rank = entry_rank(&pool, seq).await;
    let tree_size = wait_for_covering_sth(&pool, rank).await;

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
    // verified root. `seq` is a real row identifier, not a dense position —
    // never assume `seq - 1` is the leaf index (see
    // `crates/chain/src/postgres.rs`'s module doc comment on why `seq` can
    // have gaps); a real mirror derives an entry's rank the same way this
    // test does, by counting entries at or before it in its own locally
    // mirrored copy of the ledger.
    let leaf_index: i64 =
        sqlx::query("SELECT COUNT(*) - 1 AS idx FROM ledger_entries WHERE seq <= $1")
            .bind(seq)
            .fetch_one(&pool)
            .await
            .unwrap()
            .try_get("idx")
            .unwrap();
    assert!(
        merkle::verify_inclusion_proof(
            &leaf_bytes,
            leaf_index as usize,
            tree_size as usize,
            &proof,
            &root
        ),
        "inclusion proof for a real, freshly-committed entry failed independent verification"
    );
}

/// Regression test for the exact bug class this fix closes: `seq` is
/// `GENERATED ALWAYS AS IDENTITY`, and Postgres identity/sequence
/// advancement is **not** transactional — a batch commit that inserts rows
/// and then rolls back still permanently burns whatever `seq` values it
/// already allocated. Before this fix, `tree_size` was derived from
/// `last_seq` (a raw seq value) rather than the true leaf count, so any
/// commit after a gap like this would silently sign a `SignedTreeHead`
/// whose `tree_size` overstated the real number of leaves — and #211's
/// proof endpoints, trusting that mislabeled `tree_size`, would either
/// return a wrong proof or panic on an out-of-bounds array index. This test
/// reproduces the gap directly (a rolled-back transaction, exactly the real
/// failure mode) and confirms `tree_size` and inclusion-proof verification
/// both stay correct despite it.
#[tokio::test]
#[ignore]
async fn tree_size_and_inclusion_proofs_stay_correct_across_a_seq_gap() {
    let base = server_url();
    let http = reqwest::Client::new();
    let pool = test_pool().await;

    // Burn a seq value the same way a real failed/rolled-back batch commit
    // would: insert into ledger_entries, then roll back. `batch_id`'s FK is
    // DEFERRABLE INITIALLY DEFERRED (crates/server/db/migrations/0014_ledger_batches),
    // so an arbitrary, never-persisted batch_id is fine here — the
    // constraint is never actually checked, since this transaction never
    // commits.
    let mut tx = pool.begin().await.expect("begin failed");
    let burned_seq: i64 = sqlx::query(
        r#"
        INSERT INTO ledger_entries
            (event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id)
        VALUES ($1, 'test.gap_probe', 'test:gap:self', 'test:gap', '{}', now(), 1, 'deadbeef', $2, $3)
        RETURNING seq
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(format!("gap-probe-{}", Uuid::new_v4()))
    .bind(Uuid::new_v4())
    .fetch_one(&mut *tx)
    .await
    .expect("insert failed")
    .try_get("seq")
    .expect("seq column");
    tx.rollback().await.expect("rollback failed");

    // Confirm the gap actually exists: no row has this seq now, but the
    // sequence counter has still moved past it.
    let still_exists: Option<i64> = sqlx::query("SELECT seq FROM ledger_entries WHERE seq = $1")
        .bind(burned_seq)
        .fetch_optional(&pool)
        .await
        .expect("query failed")
        .map(|row| row.try_get("seq").unwrap());
    assert!(
        still_exists.is_none(),
        "expected seq {burned_seq} to be burned (no row), test setup didn't reproduce a gap"
    );

    // Commit a real entry through the normal API — its seq lands strictly
    // after the burned one.
    let issuer = register_throwaway_integrator(&http, &base).await;
    let seq = wait_for_committed_seq(&pool, &issuer).await;
    assert!(
        seq > burned_seq,
        "expected the new entry's seq ({seq}) to land after the burned one ({burned_seq})"
    );
    let rank = entry_rank(&pool, seq).await;
    let tree_size = wait_for_covering_sth(&pool, rank).await;

    // The core assertion this fix guarantees: tree_size must equal the true
    // row count, never a raw seq value that the gap has inflated past it.
    let real_count: i64 = sqlx::query("SELECT COUNT(*) AS count FROM ledger_entries")
        .fetch_one(&pool)
        .await
        .unwrap()
        .try_get("count")
        .unwrap();
    assert!(
        tree_size <= real_count,
        "tree_size ({tree_size}) must never exceed the real row count ({real_count}) — a seq gap must not inflate it"
    );

    let leaf_index: i64 =
        sqlx::query("SELECT COUNT(*) - 1 AS idx FROM ledger_entries WHERE seq <= $1")
            .bind(seq)
            .fetch_one(&pool)
            .await
            .unwrap()
            .try_get("idx")
            .unwrap();
    assert_ne!(
        leaf_index, seq - 1,
        "test setup didn't actually produce a gap before this entry — seq - 1 still happens to equal the real rank, so this test wouldn't catch a regression of the bug it targets"
    );

    // And the inclusion proof for this entry — the thing that would have
    // panicked or silently mis-verified before this fix — must still
    // succeed and independently verify.
    let sth_response: serde_json::Value = http
        .get(format!("{base}/ledger/sth/{tree_size}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let proof_response = http
        .get(format!("{base}/ledger/proof/inclusion"))
        .query(&[("seq", seq), ("tree_size", tree_size)])
        .send()
        .await
        .unwrap();
    assert!(
        proof_response.status().is_success(),
        "inclusion proof request failed despite a real, committed entry, seq={seq} tree_size={tree_size}: {:?}",
        proof_response.status()
    );
    let proof_response: serde_json::Value = proof_response.json().await.unwrap();
    assert_eq!(proof_response["root_hash"], sth_response["root_hash"]);

    let leaf_bytes = hex::decode(proof_response["leaf_hash"].as_str().unwrap()).unwrap();
    let proof: Vec<[u8; 32]> = proof_response["proof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| root_hash_bytes(h.as_str().unwrap()))
        .collect();
    let root = root_hash_bytes(sth_response["root_hash"].as_str().unwrap());

    assert!(
        merkle::verify_inclusion_proof(
            &leaf_bytes,
            leaf_index as usize,
            tree_size as usize,
            &proof,
            &root
        ),
        "inclusion proof failed to verify for an entry committed after a seq gap"
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

    let issuer = register_throwaway_integrator(&http, &base).await;
    let seq = wait_for_committed_seq(&pool, &issuer).await;
    let rank = entry_rank(&pool, seq).await;
    let second_tree_size = wait_for_covering_sth(&pool, rank).await;
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
