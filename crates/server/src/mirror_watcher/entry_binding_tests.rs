//! #1165: backfill binds fetched entry content to the verified hash chain.

use super::*;
use avalon_chain::{hash_entry, EntryContent};
use avalon_protocol::signing_bytes::SigningBytesError;
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
    let seqs: Vec<i64> = (1..=payloads.len() as i64).collect();
    genuine_ledger_at(network_id, payloads, &seqs)
}

/// A genuine ledger whose entries carry (and hash) the given `seq` labels.
fn genuine_ledger_at(network_id: &str, payloads: Vec<serde_json::Value>, seqs: &[i64]) -> Ledger {
    let count = payloads.len();
    let ts = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let who = format!("identity:{}:self:noop", Uuid::new_v4());
    let mut prev = avalon_chain::GENESIS_HASH.to_string();
    let (mut entries, mut hashes) = (Vec::new(), Vec::new());
    for (i, payload) in payloads.iter().enumerate() {
        let event_id = Uuid::new_v4();
        let payload_hash = avalon_chain::payload_hash_hex(payload).unwrap();
        let hash = hash_entry(
            network_id,
            mirror::CORE_SHARD_ID,
            &prev,
            &EntryContent {
                seq: seqs[i],
                event_id,
                kind: "test.noop",
                issuer: &who,
                subject: &who,
                payload_hash: &payload_hash,
                timestamp: ts,
                version: 1,
                envelope: &avalon_protocol::signing_bytes::Envelope::current(
                    avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
                ),
            },
        )
        .unwrap();
        entries.push(serde_json::json!({
            "seq": seqs[i], "event_id": event_id, "kind": "test.noop", "issuer": who,
            "subject": who, "payload": payload, "payload_hash": payload_hash, "payload_pruned": false, "version": 1,
            "event_timestamp": rfc3339(ts), "prev_hash": prev, "entry_hash": hash,
            "batch_id": Uuid::new_v4(),
            "layout_version": 1, "rules_version": 1, "hash_algo": 1, "extensions": "0000",
        }));
        hashes.push(hash.clone());
        prev = hash;
    }
    let root =
        hex::encode(merkle::mth_of_hex_hashes(avalon_chain::ledger_hash_algo(), &hashes).unwrap());
    let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let sth = avalon_protocol::sth::sign_tree_head(
        &key,
        "k",
        count as i64,
        &root,
        network_id,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
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
                let proof = merkle::inclusion_proof_of_hex_hashes(
                    avalon_chain::ledger_hash_algo(),
                    idx,
                    &hashes,
                )
                .unwrap();
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
        self.backfill_candidates(pool, &[server.uri()], 0).await
    }

    async fn backfill_candidates(
        &self,
        pool: &PgPool,
        candidates: &[String],
        tick: usize,
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
            tick,
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
    let forged_payload_hash = avalon_chain::payload_hash_hex(&served[1]["payload"]).unwrap();
    served[1]["payload_hash"] = serde_json::json!(forged_payload_hash);
    let forged = hash_entry(
        &net,
        mirror::CORE_SHARD_ID,
        served[1]["prev_hash"].as_str().unwrap(),
        &EntryContent {
            seq: 2,
            event_id: served[1]["event_id"].as_str().unwrap().parse().unwrap(),
            kind: "test.noop",
            issuer: served[1]["issuer"].as_str().unwrap(),
            subject: served[1]["subject"].as_str().unwrap(),
            payload_hash: &forged_payload_hash,
            timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            version: 1,
            envelope: &avalon_protocol::signing_bytes::Envelope::current(
                avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
            ),
        },
    )
    .unwrap();
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
        payload_hash: avalon_chain::payload_hash_hex(&first["payload"].clone()).unwrap(),
        event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        version: 1,
        prev_hash: avalon_chain::GENESIS_HASH.into(),
        entry_hash: ledger.hashes[0].clone(),
        batch_id: Uuid::new_v4(),
        verified_tree_size: 1,
        envelope: avalon_protocol::signing_bytes::EnvelopeWire::from(
            &avalon_protocol::signing_bytes::Envelope::current(
                avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
            ),
        ),
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
        payload_hash: avalon_chain::payload_hash_hex(&e["payload"].clone()).unwrap(),
        event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        version: 1,
        prev_hash: avalon_chain::GENESIS_HASH.into(),
        entry_hash: ledger.hashes[0].clone(),
        batch_id: Uuid::new_v4(),
        verified_tree_size: 1,
        envelope: avalon_protocol::signing_bytes::EnvelopeWire::from(
            &avalon_protocol::signing_bytes::Envelope::current(
                avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
            ),
        ),
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
        payload_hash: avalon_chain::payload_hash_hex(&v["payload"].clone()).unwrap(),
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
        envelope: avalon_protocol::signing_bytes::EnvelopeWire::from(
            &avalon_protocol::signing_bytes::Envelope::current(
                avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
            ),
        ),
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

/// Envelope fields a served entry may carry that this node cannot verify, with the typed result each must give.
fn unreadable_envelopes() -> Vec<(&'static str, serde_json::Value, SigningBytesError)> {
    use avalon_protocol::signing_bytes::VersionKind;
    let needs = |what, required| SigningBytesError::NeedsNewerVersion { what, required };
    vec![
        (
            "unknown critical extension",
            serde_json::json!({ "extensions": "000100030100000000" }),
            needs(VersionKind::CriticalExtension, 3),
        ),
        (
            "layout version above the range",
            serde_json::json!({ "layout_version": 2 }),
            needs(VersionKind::Layout, 2),
        ),
        (
            "rules version above the range",
            serde_json::json!({ "rules_version": 2 }),
            needs(VersionKind::Rules, 2),
        ),
        (
            "unknown hash algorithm",
            serde_json::json!({ "hash_algo": 9 }),
            needs(VersionKind::HashAlgo, 9),
        ),
    ]
}

/// DB-free: an entry this node cannot read is never verified, and the result is typed.
#[test]
fn an_entry_needing_a_newer_version_is_not_verified_and_gives_the_typed_result() {
    let ledger = genuine_ledger(NET, 2);
    let v = &ledger.entries[1];
    let entry_with = |patch: &serde_json::Value| {
        let mut wire = avalon_protocol::signing_bytes::EnvelopeWire::from(
            &avalon_protocol::signing_bytes::Envelope::current(
                avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
            ),
        );
        let mut as_json = serde_json::to_value(&wire).unwrap();
        for (k, val) in patch.as_object().unwrap() {
            as_json[k] = val.clone();
        }
        wire = serde_json::from_value(as_json).unwrap();
        mirror::MirroredEntry {
            source_url: "p".into(),
            network_id: NET.into(),
            shard_id: mirror::CORE_SHARD_ID.into(),
            seq: 2,
            event_id: v["event_id"].as_str().unwrap().parse().unwrap(),
            kind: v["kind"].as_str().unwrap().into(),
            issuer: v["issuer"].as_str().unwrap().into(),
            subject: v["subject"].as_str().unwrap().into(),
            payload: Some(v["payload"].clone()),
            payload_hash: avalon_chain::payload_hash_hex(&v["payload"].clone()).unwrap(),
            event_timestamp: OffsetDateTime::parse(
                v["event_timestamp"].as_str().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap(),
            version: 1,
            prev_hash: v["prev_hash"].as_str().unwrap().into(),
            entry_hash: v["entry_hash"].as_str().unwrap().into(),
            batch_id: Uuid::new_v4(),
            verified_tree_size: 2,
            envelope: wire,
        }
    };
    for (name, patch, want) in unreadable_envelopes() {
        let err = verify_entry_binding(&entry_with(&patch), &ledger.hashes[1], &ledger.hashes[0])
            .unwrap_err();
        assert!(
            matches!(&err, MirrorWatcherError::NeedsNewerVersion(e) if *e == want),
            "{name}: {err:?}"
        );
    }
}

/// An entry the node cannot verify is refused with the typed result and never stored; earlier
/// entries stay.
#[tokio::test]
#[ignore]
async fn an_entry_needing_a_newer_version_is_refused_with_the_typed_result_and_not_stored() {
    let pool = pool().await;
    for (name, patch, want) in unreadable_envelopes() {
        let net = fresh_net();
        let ledger = genuine_ledger(&net, 3);
        let mut served = ledger.entries.clone();
        for (k, val) in patch.as_object().unwrap() {
            served[1][k] = val.clone();
        }
        let server = ledger.serve(served).await;
        let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
        assert!(
            matches!(&err, MirrorWatcherError::NeedsNewerVersion(e) if *e == want),
            "{name}: {err:?}"
        );
        assert_eq!(stored_seqs(&pool, &net).await, vec![1], "{name}");
    }
}

/// The insert itself refuses an entry it cannot read, so nothing unverifiable is stored or served.
#[tokio::test]
#[ignore]
async fn inserting_an_entry_needing_a_newer_version_stores_nothing() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 1);
    let v = &ledger.entries[0];
    let mut wire = avalon_protocol::signing_bytes::EnvelopeWire::from(
        &avalon_protocol::signing_bytes::Envelope::current(
            avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
        ),
    );
    wire.extensions = "000100030100000000".to_string();
    let entry = mirror::MirroredEntry {
        source_url: "p".into(),
        network_id: net.clone(),
        shard_id: mirror::CORE_SHARD_ID.into(),
        seq: 1,
        event_id: v["event_id"].as_str().unwrap().parse().unwrap(),
        kind: v["kind"].as_str().unwrap().into(),
        issuer: v["issuer"].as_str().unwrap().into(),
        subject: v["subject"].as_str().unwrap().into(),
        payload: Some(v["payload"].clone()),
        payload_hash: avalon_chain::payload_hash_hex(&v["payload"].clone()).unwrap(),
        event_timestamp: OffsetDateTime::parse(
            v["event_timestamp"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap(),
        version: 1,
        prev_hash: v["prev_hash"].as_str().unwrap().into(),
        entry_hash: v["entry_hash"].as_str().unwrap().into(),
        batch_id: Uuid::new_v4(),
        verified_tree_size: 1,
        envelope: wire,
    };
    let err = mirror::insert_mirrored_entry(&pool, &entry)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        avalon_chain::SettlementError::NeedsNewerVersion {
            what: avalon_protocol::signing_bytes::VersionKind::CriticalExtension,
            required: 3
        }
    ));
    assert!(stored_seqs(&pool, &net).await.is_empty());
}

async fn stored_payload(pool: &PgPool, net: &str, seq: i64) -> Option<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM mirrored_entries WHERE network_id = $1 AND seq = $2")
        .bind(net)
        .bind(seq)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// #1170: a genuine entry served under a different, still increasing `seq` no longer verifies.
#[tokio::test]
#[ignore]
async fn a_genuine_entry_under_a_wrong_increasing_seq_is_refused_as_a_content_mismatch() {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 3);
    let mut served = ledger.entries.clone();
    served[2]["seq"] = serde_json::json!(5);
    let server = ledger.serve(served).await;
    let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
    assert!(
        matches!(err, MirrorWatcherError::EntryContentMismatch { seq: 5 }),
        "{err:?}"
    );
    assert_eq!(stored_seqs(&pool, &net).await, vec![1, 2]);
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
    let ledger = genuine_ledger_at(
        &net,
        (0..3).map(|i| serde_json::json!({ "n": i })).collect(),
        &[1, 5, 40],
    );
    let server = ledger.serve(ledger.entries.clone()).await;
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
            payload_hash: avalon_chain::payload_hash_hex(&e["payload"].clone()).unwrap(),
            event_timestamp: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            version: 1,
            prev_hash: avalon_chain::GENESIS_HASH.into(),
            entry_hash: l.hashes[0].clone(),
            batch_id: Uuid::new_v4(),
            verified_tree_size: 1,
            envelope: avalon_protocol::signing_bytes::EnvelopeWire::from(
                &avalon_protocol::signing_bytes::Envelope::current(
                    avalon_protocol::signing_bytes::tags::LEDGER_ENTRY,
                ),
            ),
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
    let kinds = [
        "tampered",
        "pruned",
        "bad_proof",
        "garbage_proof",
        "proof_404",
        "empty",
        "bad_seq",
        "dead",
    ];
    // Tick 0 contacts the bad candidate first; tick 1 starts at the honest one.
    for (kind, tick) in kinds.into_iter().flat_map(|k| [(k, 0usize), (k, 1)]) {
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
        if matches!(kind, "bad_proof" | "garbage_proof" | "proof_404") {
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
            let proof_response = match kind {
                "garbage_proof" => ResponseTemplate::new(200).set_body_string("not json"),
                "proof_404" => ResponseTemplate::new(404),
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "root_hash": ledger.sth.root_hash, "leaf_hash": ledger.hashes[0],
                    "proof": [],
                })),
            };
            Mock::given(matchers::path("/ledger/proof/inclusion"))
                .respond_with(proof_response)
                .mount(&bad_server)
                .await;
        }
        let good = ledger.serve(ledger.entries.clone()).await;
        let first = if kind == "dead" {
            dead.clone()
        } else {
            bad_server.uri()
        };
        ledger
            .backfill_candidates(&pool, &[first.clone(), good.uri()], tick)
            .await
            .unwrap_or_else(|e| panic!("{kind}/{tick}: {e:?}"));
        assert_eq!(
            stored_seqs(&pool, &net).await,
            vec![1, 2, 3, 4],
            "{kind}/{tick}"
        );
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
        .backfill_candidates(&pool, &[sa.uri(), sb.uri()], 0)
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

#[tokio::test]
#[ignore]
async fn every_candidate_serving_garbage_proofs_is_a_refusal_not_an_outage() {
    let pool = pool().await;
    for response in [
        ResponseTemplate::new(200).set_body_string("not json"),
        ResponseTemplate::new(404),
    ] {
        let net = fresh_net();
        let ledger = genuine_ledger(&net, 2);
        let server = ledger.serve(ledger.entries.clone()).await;
        server.reset().await;
        let entries = ledger.entries.clone();
        Mock::given(matchers::path("/ledger/entries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(entries))
            .mount(&server)
            .await;
        Mock::given(matchers::path("/ledger/proof/inclusion"))
            .respond_with(response)
            .mount(&server)
            .await;
        let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
        assert!(
            matches!(
                err,
                MirrorWatcherError::Decode(_) | MirrorWatcherError::Http(_)
            ),
            "{err:?}"
        );
        assert!(stored_seqs(&pool, &net).await.is_empty());
    }
}

#[tokio::test]
#[ignore]
async fn the_seq_bound_is_exact_and_a_first_seq_above_a_million_is_accepted() {
    let pool = pool().await;
    // (first label, second label, expected outcome for the second)
    let net = fresh_net();
    let ledger = genuine_ledger_at(
        &net,
        vec![serde_json::json!({ "n": 0 }), serde_json::json!({ "n": 1 })],
        &[5_000_000, 5_000_000 + MAX_SEQ_GAP],
    );
    let server = ledger.serve(ledger.entries.clone()).await;
    ledger.backfill_from(&pool, &server).await.unwrap();
    assert_eq!(
        stored_seqs(&pool, &net).await,
        vec![5_000_000, 5_000_000 + MAX_SEQ_GAP]
    );

    let net = fresh_net();
    let ledger = genuine_ledger_at(
        &net,
        vec![serde_json::json!({ "n": 0 }), serde_json::json!({ "n": 1 })],
        &[1, 1 + MAX_SEQ_GAP + 1],
    );
    let server = ledger.serve(ledger.entries.clone()).await;
    let err = ledger.backfill_from(&pool, &server).await.unwrap_err();
    assert!(matches!(err, MirrorWatcherError::EntrySeqInvalid { .. }));
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);
}

/// A hostile `Retry-After` on `path` is clamped, so a throttled backfill finishes in about the
/// clamp (real time, ~30s) rather than the claimed day.
async fn assert_retry_after_is_clamped(path: &str) {
    let pool = pool().await;
    let net = fresh_net();
    let ledger = genuine_ledger(&net, 1);
    let server = ledger.serve(ledger.entries.clone()).await;
    Mock::given(matchers::path(path))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "86400"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    tokio::time::timeout(
        MAX_RETRY_AFTER + Duration::from_secs(15),
        ledger.backfill_from(&pool, &server),
    )
    .await
    .expect("backfill waited past the Retry-After clamp")
    .unwrap();
    assert_eq!(stored_seqs(&pool, &net).await, vec![1]);
}

#[tokio::test]
#[ignore]
async fn backfill_clamps_a_hostile_retry_after_on_the_entries_request() {
    assert_retry_after_is_clamped("/ledger/entries").await;
}

#[tokio::test]
#[ignore]
async fn backfill_clamps_a_hostile_retry_after_on_the_proof_request() {
    assert_retry_after_is_clamped("/ledger/proof/inclusion").await;
}
