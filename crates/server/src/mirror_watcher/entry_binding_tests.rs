//! #1165: backfill binds fetched entry content to the verified hash chain.

use super::*;
use avalon_chain::{hash_entry, EntryContent};
use wiremock::{matchers, Mock, MockServer, Request, ResponseTemplate};

const NET: &str = "avalon-test-entry-binding";

/// A genuine ledger: real hash chain, root and per-entry inclusion proofs.
struct Ledger {
    entries: Vec<serde_json::Value>,
    hashes: Vec<String>,
    sth: SignedTreeHead,
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

fn genuine_ledger(network_id: &str, count: usize) -> Ledger {
    genuine_ledger_with(
        network_id,
        (0..count).map(|i| serde_json::json!({ "n": i })).collect(),
    )
}

fn genuine_ledger_with(network_id: &str, payloads: Vec<serde_json::Value>) -> Ledger {
    let count = payloads.len();
    let ts = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let who = format!("identity:{}:self:noop", Uuid::new_v4());
    let mut prev = avalon_chain::GENESIS_HASH.to_string();
    let (mut entries, mut hashes) = (Vec::new(), Vec::new());
    for (i, payload) in payloads.iter().enumerate() {
        let event_id = Uuid::new_v4();
        let hash = hash_entry(
            network_id,
            &prev,
            &EntryContent {
                event_id,
                kind: "test.noop",
                issuer: &who,
                subject: &who,
                payload,
                timestamp: ts,
                version: 1,
            },
        );
        entries.push(serde_json::json!({
            "seq": i + 1, "event_id": event_id, "kind": "test.noop", "issuer": who,
            "subject": who, "payload": payload, "payload_pruned": false, "version": 1,
            "event_timestamp": rfc3339(ts), "prev_hash": prev, "entry_hash": hash,
            "batch_id": Uuid::new_v4(),
        }));
        hashes.push(hash.clone());
        prev = hash;
    }
    let root = hex::encode(merkle::mth_of_hex_hashes(&hashes).unwrap());
    let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let sth = avalon_protocol::sth::sign_tree_head(
        &key,
        "k",
        count as i64,
        &root,
        network_id,
        OffsetDateTime::now_utc(),
    );
    Ledger {
        entries,
        hashes,
        sth,
    }
}

impl Ledger {
    /// Serves `entries` (possibly tampered) with the genuine head's proofs.
    async fn serve(&self, entries: Vec<serde_json::Value>) -> MockServer {
        let served_labels: Vec<i64> = entries.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/ledger/entries"))
            .respond_with(move |req: &Request| {
                let since: i64 = req
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "since_seq")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                let page: Vec<_> = entries
                    .iter()
                    .filter(|e| e["seq"].as_i64().unwrap() > since)
                    .collect();
                ResponseTemplate::new(200).set_body_json(page)
            })
            .mount(&server)
            .await;
        let (hashes, root) = (self.hashes.clone(), self.sth.root_hash.clone());
        let labels: Vec<i64> = served_labels;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/ledger/proof/inclusion"))
            .respond_with(move |req: &Request| {
                let seq: i64 = req
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "seq")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap();
                // The entry served under that label is the one proven, as a source would answer.
                let idx = labels.iter().position(|l| *l == seq).unwrap();
                let proof = merkle::inclusion_proof_of_hex_hashes(idx, &hashes).unwrap();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "root_hash": root, "leaf_hash": hashes[idx],
                    "proof": proof.iter().map(hex::encode).collect::<Vec<_>>(),
                }))
            })
            .mount(&server)
            .await;
        server
    }

    async fn backfill_from(
        &self,
        pool: &PgPool,
        server: &MockServer,
    ) -> Result<(), MirrorWatcherError> {
        self.backfill_candidates(pool, &[server.uri()]).await
    }

    async fn backfill_candidates(
        &self,
        pool: &PgPool,
        candidates: &[String],
    ) -> Result<(), MirrorWatcherError> {
        backfill(
            &crate::node_http::NodeClient::new(),
            pool,
            &PostgresIndexer::new(pool.clone()),
            mirror::CORE_SHARD_ID,
            candidates,
            &self.sth,
            None,
            false,
        )
        .await
    }
}

async fn pool() -> PgPool {
    avalon_devenv::load();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    sqlx::postgres::PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("failed to connect to Postgres")
}

async fn stored_seqs(pool: &PgPool, network_id: &str) -> Vec<i64> {
    sqlx::query_scalar("SELECT seq FROM mirrored_entries WHERE network_id = $1 ORDER BY seq")
        .bind(network_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

fn fresh_net() -> String {
    format!("{NET}-{}", Uuid::new_v4())
}

fn tampers() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("payload", serde_json::json!({ "n": 999 })),
        ("kind", serde_json::json!("identity.created")),
        ("issuer", serde_json::json!("identity:forged:self:noop")),
        ("subject", serde_json::json!("identity:forged:self:noop")),
        ("event_timestamp", serde_json::json!("2030-01-01T00:00:00Z")),
        ("version", serde_json::json!(2)),
        ("event_id", serde_json::json!(Uuid::new_v4())),
    ]
}

#[tokio::test]
#[ignore]
async fn a_genuine_ledger_is_stored_in_full() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 3);
    let server = ledger.serve(ledger.entries.clone()).await;
    ledger.backfill_from(&pool, &server).await.unwrap();
    assert_eq!(stored_seqs(&pool, &net).await, vec![1, 2, 3]);
}

#[tokio::test]
#[ignore]
async fn tampered_content_with_genuine_hashes_and_proofs_is_refused_and_not_stored() {
    let pool = pool().await;
    for (field, forged) in tampers() {
        let net = fresh_net();
        let ledger = genuine_ledger(&net, 3);
        let mut served = ledger.entries.clone();
        served[1][field] = forged;
        let server = ledger.serve(served).await;
        let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
        assert!(
            matches!(err, MirrorWatcherError::EntryContentMismatch { seq: 2 }),
            "{field}: {err:?}"
        );
        // The genuine entry before the forged one is kept; nothing at or after it is.
        assert_eq!(stored_seqs(&pool, &net).await, vec![1], "{field}");
    }
}

#[tokio::test]
#[ignore]
async fn a_forged_first_entry_stores_nothing() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 2);
    let mut served = ledger.entries.clone();
    served[0]["payload"] = serde_json::json!({ "n": 42 });
    let server = ledger.serve(served).await;
    let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryContentMismatch { seq: 1 }
    ));
    assert!(stored_seqs(&pool, &net).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn forged_content_with_a_recomputed_hash_fails_the_proof_leaf_check() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 2);
    let mut served = ledger.entries.clone();
    served[1]["payload"] = serde_json::json!({ "n": 7 });
    let forged = hash_entry(
        &net,
        served[1]["prev_hash"].as_str().unwrap(),
        &EntryContent {
            event_id: served[1]["event_id"].as_str().unwrap().parse().unwrap(),
            kind: "test.noop",
            issuer: served[1]["issuer"].as_str().unwrap(),
            subject: served[1]["subject"].as_str().unwrap(),
            payload: &served[1]["payload"],
            timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            version: 1,
        },
    );
    served[1]["entry_hash"] = serde_json::json!(forged);
    let server = ledger.serve(served).await;
    let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::InvalidInclusionProof { seq: 2, .. }
    ));
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);
}

#[tokio::test]
#[ignore]
async fn a_wrong_prev_hash_is_refused_and_not_stored() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 3);
    let mut served = ledger.entries.clone();
    served[1]["prev_hash"] = serde_json::json!("ab".repeat(32));
    let server = ledger.serve(served).await;
    let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryChainBroken { seq: 2 }
    ));
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);
}

#[tokio::test]
#[ignore]
async fn a_first_entry_must_link_to_genesis() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 2);
    let mut served = ledger.entries.clone();
    served[0]["prev_hash"] = serde_json::json!("ab".repeat(32));
    let server = ledger.serve(served).await;
    let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryChainBroken { seq: 1 }
    ));
    assert!(stored_seqs(&pool, &net).await.is_empty());
}

#[tokio::test]
#[ignore]
async fn a_resumed_backfill_links_to_the_stored_entry_and_refuses_a_bad_link() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 3);
    let server = ledger.serve(ledger.entries.clone()).await;
    // Pre-store entry 1 as an earlier pass would have.
    let first = &ledger.entries[0];
    let stored = mirror::MirroredEntry {
        source_url: server.uri(),
        network_id: net.clone(),
        shard_id: mirror::CORE_SHARD_ID.to_string(),
        seq: 1,
        event_id: first["event_id"].as_str().unwrap().parse().unwrap(),
        kind: "test.noop".into(),
        issuer: first["issuer"].as_str().unwrap().into(),
        subject: first["subject"].as_str().unwrap().into(),
        payload: Some(first["payload"].clone()),
        event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        version: 1,
        prev_hash: avalon_chain::GENESIS_HASH.into(),
        entry_hash: ledger.hashes[0].clone(),
        batch_id: Uuid::new_v4(),
        verified_tree_size: 1,
    };
    mirror::insert_mirrored_entry(&pool, &stored).await.unwrap();

    let mut bad = ledger.entries.clone();
    bad[1]["prev_hash"] = serde_json::json!("cd".repeat(32));
    let bad_server = ledger.serve(bad).await;
    let err = ledger.backfill_from(&pool, &bad_server).await.unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryChainBroken { seq: 2 }
    ));
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);

    ledger.backfill_from(&pool, &server).await.unwrap();
    assert_eq!(stored_seqs(&pool, &net).await, vec![1, 2, 3]);
}

#[tokio::test]
#[ignore]
async fn a_pruned_payload_is_refused_and_not_stored() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 2);
    let mut served = ledger.entries.clone();
    served[1]["payload"] = serde_json::Value::Null;
    served[1]["payload_pruned"] = serde_json::json!(true);
    let server = ledger.serve(served).await;
    let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryPayloadPruned { seq: 2 }
    ));
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);
}

#[tokio::test]
#[ignore]
async fn storage_refuses_content_that_does_not_hash_to_the_claimed_hash() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 1);
    let e = &ledger.entries[0];
    let mut entry = mirror::MirroredEntry {
        source_url: "http://peer".into(),
        network_id: net.clone(),
        shard_id: mirror::CORE_SHARD_ID.into(),
        seq: 1,
        event_id: e["event_id"].as_str().unwrap().parse().unwrap(),
        kind: "test.noop".into(),
        issuer: e["issuer"].as_str().unwrap().into(),
        subject: e["subject"].as_str().unwrap().into(),
        payload: Some(e["payload"].clone()),
        event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        version: 1,
        prev_hash: avalon_chain::GENESIS_HASH.into(),
        entry_hash: ledger.hashes[0].clone(),
        batch_id: Uuid::new_v4(),
        verified_tree_size: 1,
    };
    entry.kind = "identity.created".into();
    let err = mirror::insert_mirrored_entry(&pool, &entry)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        avalon_chain::SettlementError::MirroredContentMismatch { seq: 1 }
    ));
    entry.kind = "test.noop".into();
    entry.payload = None;
    let err = mirror::insert_mirrored_entry(&pool, &entry)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        avalon_chain::SettlementError::MirroredPayloadUnverifiable { seq: 1 }
    ));
    assert!(stored_seqs(&pool, &net).await.is_empty());
}

/// DB-free: the binding check alone, over each tampered field and the link.
#[test]
fn binding_check_rejects_each_tampered_field_and_a_bad_link() {
    let ledger = genuine_ledger(NET, 2);
    let entry_of = |v: &serde_json::Value| mirror::MirroredEntry {
        source_url: "p".into(),
        network_id: NET.into(),
        shard_id: mirror::CORE_SHARD_ID.into(),
        seq: v["seq"].as_i64().unwrap(),
        event_id: v["event_id"].as_str().unwrap().parse().unwrap(),
        kind: v["kind"].as_str().unwrap().into(),
        issuer: v["issuer"].as_str().unwrap().into(),
        subject: v["subject"].as_str().unwrap().into(),
        payload: Some(v["payload"].clone()),
        event_timestamp: OffsetDateTime::parse(
            v["event_timestamp"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap(),
        version: v["version"].as_i64().unwrap() as i32,
        prev_hash: v["prev_hash"].as_str().unwrap().into(),
        entry_hash: v["entry_hash"].as_str().unwrap().into(),
        batch_id: Uuid::new_v4(),
        verified_tree_size: 2,
    };
    let leaf = ledger.hashes[1].clone();
    let prev = ledger.hashes[0].clone();
    let genuine = entry_of(&ledger.entries[1]);
    assert!(verify_entry_binding(&genuine, &leaf, &prev).is_ok());

    for (field, forged) in tampers() {
        let mut v = ledger.entries[1].clone();
        v[field] = forged;
        let err = verify_entry_binding(&entry_of(&v), &leaf, &prev).unwrap_err();
        assert!(
            matches!(err, MirrorWatcherError::EntryContentMismatch { seq: 2 }),
            "{field}: {err:?}"
        );
    }
    let err = verify_entry_binding(&genuine, &leaf, &"00".repeat(32)).unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryChainBroken { seq: 2 }
    ));
    let err = verify_entry_binding(&genuine, &"11".repeat(32), &prev).unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryContentMismatch { seq: 2 }
    ));
    let mut pruned = genuine;
    pruned.payload = None;
    let err = verify_entry_binding(&pruned, &leaf, &prev).unwrap_err();
    assert!(matches!(
        err,
        MirrorWatcherError::EntryPayloadPruned { seq: 2 }
    ));
}

async fn stored_payload(pool: &PgPool, net: &str, seq: i64) -> Option<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM mirrored_entries WHERE network_id = $1 AND seq = $2")
        .bind(net)
        .bind(seq)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn a_forged_seq_label_is_refused_not_silently_dropped() {
    let pool = pool().await;
    // (label for the third served entry, why it is refused)
    for (label, why) in [
        (2i64, "a seq already stored"),
        (9_000_000_000, "a huge seq"),
        (1, "a lower seq"),
    ] {
        let net = fresh_net();
        let ledger = genuine_ledger(&net, 3);
        let mut served = ledger.entries.clone();
        served[2]["seq"] = serde_json::json!(label);
        let server = ledger.serve(served).await;
        let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
        assert!(
            matches!(err, MirrorWatcherError::EntrySeqInvalid { seq } if seq == label),
            "{why}: {err:?}"
        );
        assert_eq!(stored_seqs(&pool, &net).await, vec![1, 2], "{why}");
    }
}

#[tokio::test]
#[ignore]
async fn seq_gaps_are_accepted_but_must_increase() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 3);
    let mut served = ledger.entries.clone();
    served[1]["seq"] = serde_json::json!(5);
    served[2]["seq"] = serde_json::json!(40);
    let server = ledger.serve(served).await;
    ledger.backfill_from(&pool, &server).await.unwrap();
    assert_eq!(stored_seqs(&pool, &net).await, vec![1, 5, 40]);
}

#[tokio::test]
#[ignore]
async fn a_conflicting_insert_is_an_error_and_advances_nothing() {
    let pool = pool().await;
    let net = fresh_net();
    let a = genuine_ledger(&net, 1);
    let b = genuine_ledger(&net, 1);
    let to_entry = |l: &Ledger| {
        let e = &l.entries[0];
        mirror::MirroredEntry {
            source_url: "p".into(),
            network_id: net.clone(),
            shard_id: mirror::CORE_SHARD_ID.into(),
            seq: 1,
            event_id: e["event_id"].as_str().unwrap().parse().unwrap(),
            kind: "test.noop".into(),
            issuer: e["issuer"].as_str().unwrap().into(),
            subject: e["subject"].as_str().unwrap().into(),
            payload: Some(e["payload"].clone()),
            event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            version: 1,
            prev_hash: avalon_chain::GENESIS_HASH.into(),
            entry_hash: l.hashes[0].clone(),
            batch_id: Uuid::new_v4(),
            verified_tree_size: 1,
        }
    };
    let (first, second) = (to_entry(&a), to_entry(&b));
    assert!(mirror::insert_mirrored_entry(&pool, &first).await.unwrap());
    assert!(!mirror::insert_mirrored_entry(&pool, &second).await.unwrap());
    let indexer = PostgresIndexer::new(pool.clone());
    let mut blocked = false;
    let err = store_and_project(&pool, &indexer, &second, &mut blocked)
        .await
        .unwrap_err();
    assert!(matches!(err, MirrorWatcherError::EntryConflict { seq: 1 }));
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);
}

#[tokio::test]
#[ignore]
async fn a_bad_or_pruned_first_candidate_fails_over_to_an_honest_one_in_one_tick() {
    let pool = pool().await;
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    for kind in [
        "tampered",
        "pruned",
        "bad_proof",
        "empty",
        "bad_seq",
        "dead",
    ] {
        let net = fresh_net();
        let ledger = genuine_ledger(&net, 4);
        let mut bad = ledger.entries.clone();
        match kind {
            "tampered" => bad[1]["payload"] = serde_json::json!({ "n": 77 }),
            "pruned" => {
                bad[2]["payload"] = serde_json::Value::Null;
                bad[2]["payload_pruned"] = serde_json::json!(true);
            }
            "bad_seq" => bad[1]["seq"] = serde_json::json!(1),
            "empty" => bad.clear(),
            _ => {}
        }
        let bad_server = ledger.serve(bad).await;
        if kind == "bad_proof" {
            // Honest entries, but this source's proofs are for another leaf.
            bad_server.reset().await;
            let entries = ledger.entries.clone();
            Mock::given(matchers::path("/ledger/entries"))
                .respond_with(move |req: &Request| {
                    let since: i64 = req
                        .url
                        .query_pairs()
                        .find(|(k, _)| k == "since_seq")
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    let page: Vec<_> = entries
                        .iter()
                        .filter(|e| e["seq"].as_i64().unwrap() > since)
                        .collect();
                    ResponseTemplate::new(200).set_body_json(page)
                })
                .mount(&bad_server)
                .await;
            Mock::given(matchers::path("/ledger/proof/inclusion"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "root_hash": ledger.sth.root_hash, "leaf_hash": ledger.hashes[0],
                    "proof": [],
                })))
                .mount(&bad_server)
                .await;
        }
        let good = ledger.serve(ledger.entries.clone()).await;
        let first = if kind == "dead" {
            dead.clone()
        } else {
            bad_server.uri()
        };
        // Both orders: the starting candidate rotates per tick, so either may go first.
        ledger
            .backfill_candidates(&pool, &[first.clone(), good.uri()])
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        assert_eq!(stored_seqs(&pool, &net).await, vec![1, 2, 3, 4], "{kind}");
        let hashes: Vec<String> = sqlx::query_scalar(
            "SELECT entry_hash FROM mirrored_entries WHERE network_id = $1 ORDER BY seq",
        )
        .bind(&net)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            hashes, ledger.hashes,
            "{kind}: stored content must be the honest one"
        );
    }
}

#[tokio::test]
#[ignore]
async fn every_candidate_failing_is_an_error_and_stores_nothing_bad() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 3);
    let mut a = ledger.entries.clone();
    a[1]["payload"] = serde_json::json!({ "n": 5 });
    let mut b = ledger.entries.clone();
    b[1]["payload"] = serde_json::Value::Null;
    b[1]["payload_pruned"] = serde_json::json!(true);
    let (sa, sb) = (ledger.serve(a).await, ledger.serve(b).await);
    let err = ledger
        .backfill_candidates(&pool, &[sa.uri(), sb.uri()])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MirrorWatcherError::EntryContentMismatch { seq: 2 }
                | MirrorWatcherError::EntryPayloadPruned { seq: 2 }
        ),
        "{err:?}"
    );
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);
}

#[tokio::test]
#[ignore]
async fn a_json_null_payload_that_is_not_pruned_is_stored_as_null() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger_with(
        &net,
        vec![
            serde_json::json!({ "n": 0 }),
            serde_json::Value::Null,
            serde_json::json!({ "n": 2 }),
        ],
    );
    let server = ledger.serve(ledger.entries.clone()).await;
    ledger.backfill_from(&pool, &server).await.unwrap();
    assert_eq!(stored_seqs(&pool, &net).await, vec![1, 2, 3]);
    assert_eq!(
        stored_payload(&pool, &net, 2).await,
        Some(serde_json::Value::Null)
    );
}
