//! A configured self-certifying (`node:<hash>`) or core shard mirrored over http(s) and over a
//! mocked `p2p://` stream: every outcome must be the same on both transports (#1147).

use super::*;
use ed25519_dalek::SigningKey;
use std::sync::{Arc, Mutex};

const NETWORK: &str = "p2p-source-test-net";

#[derive(Clone, Copy, Debug)]
enum Transport {
    Http,
    Stream,
}

/// A tiny signed ledger served by whichever transport a test picks.
struct Origin {
    signing: SigningKey,
    shard_id: String,
    hashes: Vec<String>,
    payload_bytes: usize,
    corrupt_signature: bool,
    key_override: Option<SigningKey>,
}

type SharedOrigin = Arc<Mutex<Origin>>;

fn origin(entries: usize) -> SharedOrigin {
    let signing = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
    let shard_id =
        avalon_protocol::shard_identity::derive_self_certifying_id(&signing.verifying_key());
    Arc::new(Mutex::new(Origin {
        signing,
        shard_id,
        hashes: (0..entries)
            .map(|_| hex::encode(rand::random::<[u8; 32]>()))
            .collect(),
        payload_bytes: 8,
        corrupt_signature: false,
        key_override: None,
    }))
}

fn shard_id(o: &SharedOrigin) -> String {
    o.lock().unwrap().shard_id.clone()
}

impl Origin {
    fn head(&self, size: usize) -> serde_json::Value {
        let root = hex::encode(merkle::mth_of_hex_hashes(&self.hashes[..size]).unwrap());
        let sth = avalon_protocol::sth::sign_tree_head(
            &self.signing,
            "k",
            size as i64,
            &root,
            NETWORK,
            OffsetDateTime::now_utc(),
        );
        let shown = self.key_override.as_ref().unwrap_or(&self.signing);
        let signature = if self.corrupt_signature {
            "00".repeat(64)
        } else {
            sth.signature
        };
        serde_json::json!({
            "tree_size": sth.tree_size,
            "root_hash": sth.root_hash,
            "network_id": sth.network_id,
            "signing_key_id": sth.signing_key_id,
            "signature": signature,
            "created_at": sth.created_at.format(&time::format_description::well_known::Rfc3339).unwrap(),
            "protocol_version": crate::version::PROTOCOL_VERSION,
            "signing_public_key": hex::encode(shown.verifying_key().to_bytes()),
        })
    }

    fn respond(&self, path_and_query: &str) -> (u16, serde_json::Value) {
        let url = url::Url::parse(&format!("http://origin{path_and_query}")).unwrap();
        let q = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.to_string())
        };
        if q("shard_id").is_some_and(|s| s != self.shard_id) {
            return (404, serde_json::json!({"error": "unknown shard"}));
        }
        let n = self.hashes.len();
        match url.path() {
            "/ledger/sth/latest" => (200, self.head(n)),
            "/ledger/entries" => {
                let since: usize = q("since_seq").and_then(|v| v.parse().ok()).unwrap_or(0);
                let limit: usize = q("limit").and_then(|v| v.parse().ok()).unwrap_or(100);
                let page: Vec<_> = (since + 1..=n)
                    .take(limit)
                    .map(|seq| {
                        serde_json::json!({
                            "seq": seq,
                            "event_id": Uuid::from_u128(seq as u128),
                            "kind": "k",
                            "issuer": "i",
                            "subject": "s",
                            "payload": {"p": "x".repeat(self.payload_bytes)},
                            "version": 1,
                            "event_timestamp": "2026-01-01T00:00:00Z",
                            "prev_hash": "",
                            "entry_hash": self.hashes[seq - 1],
                            "batch_id": Uuid::nil(),
                        })
                    })
                    .collect();
                (200, serde_json::Value::Array(page))
            }
            "/ledger/proof/inclusion" => {
                let seq: usize = q("seq").unwrap().parse().unwrap();
                let size: usize = q("tree_size").unwrap().parse().unwrap();
                let prefix = &self.hashes[..size];
                let proof = merkle::inclusion_proof_of_hex_hashes(seq - 1, prefix).unwrap();
                (
                    200,
                    serde_json::json!({
                        "root_hash": hex::encode(merkle::mth_of_hex_hashes(prefix).unwrap()),
                        "leaf_hash": self.hashes[seq - 1],
                        "proof": proof.iter().map(hex::encode).collect::<Vec<_>>(),
                    }),
                )
            }
            _ => (404, serde_json::json!({"error": "no route"})),
        }
    }
}

/// A client and the source string a config would name for `origin` over `transport`.
struct Served {
    client: crate::node_http::NodeClient,
    source: String,
    _http: Option<wiremock::MockServer>,
}

async fn serve(origin: &SharedOrigin, transport: Transport) -> Served {
    match transport {
        Transport::Http => {
            let server = wiremock::MockServer::start().await;
            let o = origin.clone();
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .respond_with(move |req: &wiremock::Request| {
                    let pq = match req.url.query() {
                        Some(q) => format!("{}?{q}", req.url.path()),
                        None => req.url.path().to_string(),
                    };
                    let (status, body) = o.lock().unwrap().respond(&pq);
                    wiremock::ResponseTemplate::new(status).set_body_json(body)
                })
                .mount(&server)
                .await;
            Served {
                client: crate::node_http::NodeClient::new(),
                source: server.uri(),
                _http: Some(server),
            }
        }
        Transport::Stream => {
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            let o = origin.clone();
            tokio::spawn(async move {
                while let Some(command) = rx.recv().await {
                    if let crate::dht::DhtCommand::HttpRequest {
                        request,
                        respond_to,
                        ..
                    } = command
                    {
                        let (status, body) = o.lock().unwrap().respond(&request.path_and_query);
                        let _ = respond_to.send(Ok(crate::node_http::NodeHttpResponse {
                            status,
                            headers: vec![("content-type".into(), "application/json".into())],
                            body: serde_json::to_vec(&body).unwrap(),
                        }));
                    }
                }
            });
            let handle = crate::node_http::StreamHandle {
                commands: tx,
                peers: None,
                settings: crate::node_http::NodeHttpSettings::default(),
            };
            Served {
                client: crate::node_http::NodeClient::new().with_stream(handle),
                source: crate::node_http::p2p_base_url(&libp2p::PeerId::random()),
                _http: None,
            }
        }
    }
}

const BOTH: [Transport; 2] = [Transport::Http, Transport::Stream];

async fn live_pool() -> PgPool {
    avalon_devenv::load();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    sqlx::postgres::PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("failed to connect to Postgres")
}

/// A pool that is never connected, for paths that must not touch the database.
fn dead_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap()
}

async fn head_for(
    s: &Served,
    pool: &PgPool,
    shard: &str,
    bounds: &crate::self_certifying_keys::MirrorBounds,
) -> Result<Option<(CosignedTreeHead, VerifyingKey)>, MirrorWatcherError> {
    fetch_and_verify_configured_head(
        &s.client,
        pool,
        NETWORK,
        &[],
        shard,
        &s.source,
        bounds,
        &mut 0,
    )
    .await
}

fn is_rejection(
    r: &Result<Option<(CosignedTreeHead, VerifyingKey)>, MirrorWatcherError>,
    want: crate::self_certifying_keys::Rejection,
) -> bool {
    matches!(r, Err(MirrorWatcherError::SelfCertifyingRejected(got)) if *got == want)
}

async fn stored(pool: &PgPool, shard: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM mirrored_entries WHERE shard_id = $1")
        .bind(shard)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn backfill_once(
    s: &Served,
    pool: &PgPool,
    shard: &str,
    head: &CosignedTreeHead,
    limits: BackfillLimits,
) -> Result<(), MirrorWatcherError> {
    backfill_network(
        &s.client,
        pool,
        &PostgresIndexer::new(pool.clone()),
        NETWORK,
        shard,
        &[(s.source.clone(), head.sth.clone())],
        Some(limits),
    )
    .await
}

fn limits(max_entries: usize) -> BackfillLimits {
    BackfillLimits {
        max_entries,
        deadline: std::time::Instant::now() + Duration::from_secs(30),
    }
}

#[tokio::test]
#[ignore]
async fn a_configured_self_certifying_shard_mirrors_head_entries_and_proofs_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(5);
        let shard = shard_id(&o);
        let s = serve(&o, transport).await;
        let (head, key) = head_for(&s, &pool, &shard, &bounds)
            .await
            .unwrap_or_else(|e| panic!("{transport:?}: {e}"))
            .expect("head accepted");
        assert_eq!(head.sth.tree_size, 5);
        assert_eq!(key, o.lock().unwrap().signing.verifying_key());
        backfill_once(&s, &pool, &shard, &head, limits(100))
            .await
            .unwrap();
        assert_eq!(stored(&pool, &shard).await, 5, "{transport:?}");
    }
}

#[tokio::test]
#[ignore]
async fn a_key_that_does_not_hash_to_the_shard_id_is_refused_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(2);
        let shard = shard_id(&o);
        o.lock().unwrap().key_override = Some(SigningKey::from_bytes(&rand::random()));
        let s = serve(&o, transport).await;
        let r = head_for(&s, &pool, &shard, &bounds).await;
        assert!(
            is_rejection(
                &r,
                crate::self_certifying_keys::Rejection::KeyDoesNotMatchId
            ),
            "{transport:?}: {r:?}"
        );
        assert_eq!(stored(&pool, &shard).await, 0);
    }
}

#[tokio::test]
#[ignore]
async fn a_bad_head_signature_is_refused_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(2);
        let shard = shard_id(&o);
        o.lock().unwrap().corrupt_signature = true;
        let s = serve(&o, transport).await;
        let r = head_for(&s, &pool, &shard, &bounds).await;
        assert!(
            is_rejection(&r, crate::self_certifying_keys::Rejection::BadSignature),
            "{transport:?}: {r:?}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn two_sources_signing_different_roots_are_an_equivocation_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(3);
        let shard = shard_id(&o);
        // A second source serving the same shard and key, with a different last entry.
        let fork = {
            let g = o.lock().unwrap();
            let mut hashes = g.hashes.clone();
            hashes[2] = hex::encode(rand::random::<[u8; 32]>());
            Arc::new(Mutex::new(Origin {
                signing: g.signing.clone(),
                shard_id: g.shard_id.clone(),
                hashes,
                payload_bytes: g.payload_bytes,
                corrupt_signature: false,
                key_override: None,
            }))
        };
        let chain = PostgresSettlementProvider::new(pool.clone(), NETWORK.to_string());
        let mut heads = Vec::new();
        for served in [serve(&o, transport).await, serve(&fork, transport).await] {
            let (head, _) = head_for(&served, &pool, &shard, &bounds)
                .await
                .unwrap()
                .unwrap();
            let observed =
                ObservedSth::from_sth(&served.source, &shard, &head.sth, OffsetDateTime::now_utc());
            assert!(mirror::insert_observation(&pool, &observed).await.unwrap());
            check_equivocation(&pool, &chain, "core", &observed)
                .await
                .unwrap();
            heads.push((served, head));
        }
        assert_ne!(heads[0].1.sth.root_hash, heads[1].1.sth.root_hash);
        let found = mirror::unresolved_equivocations(&pool, NETWORK, &shard)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "{transport:?}");
        // Backfill refuses to store anything while the finding stands.
        let (served, head) = &heads[0];
        backfill_once(served, &pool, &shard, head, limits(100))
            .await
            .unwrap();
        assert_eq!(stored(&pool, &shard).await, 0, "{transport:?}");
    }
}

#[tokio::test]
#[ignore]
async fn an_oversize_entry_stops_the_shard_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(2);
        let shard = shard_id(&o);
        o.lock().unwrap().payload_bytes = MAX_SELF_CERTIFYING_ENTRY_BYTES;
        let s = serve(&o, transport).await;
        let (head, _) = head_for(&s, &pool, &shard, &bounds).await.unwrap().unwrap();
        let r = backfill_once(&s, &pool, &shard, &head, limits(100)).await;
        assert!(
            matches!(r, Err(MirrorWatcherError::EntryTooLarge { seq: 1 })),
            "{transport:?}: {r:?}"
        );
        assert_eq!(stored(&pool, &shard).await, 0);
    }
}

#[tokio::test]
#[ignore]
async fn entries_beyond_the_tick_budget_wait_for_the_next_tick_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(5);
        let shard = shard_id(&o);
        let s = serve(&o, transport).await;
        let (head, _) = head_for(&s, &pool, &shard, &bounds).await.unwrap().unwrap();
        backfill_once(&s, &pool, &shard, &head, limits(2))
            .await
            .unwrap();
        assert_eq!(stored(&pool, &shard).await, 2, "{transport:?}");
        backfill_once(&s, &pool, &shard, &head, limits(100))
            .await
            .unwrap();
        assert_eq!(stored(&pool, &shard).await, 5, "{transport:?}");
    }
}

#[tokio::test]
#[ignore]
async fn the_per_source_cap_counts_the_source_string_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds {
        max_shards_per_source: 1,
        ..Default::default()
    };
    for transport in BOTH {
        let (pinned, fresh) = (origin(1), origin(1));
        let s = serve(&fresh, transport).await;
        // Another shard is already pinned under this very source string.
        let key = pinned.lock().unwrap().signing.verifying_key();
        crate::self_certifying_keys::pin_key(&pool, &shard_id(&pinned), &key, &s.source, &bounds)
            .await
            .unwrap();
        let r = head_for(&s, &pool, &shard_id(&fresh), &bounds).await;
        assert!(
            is_rejection(&r, crate::self_certifying_keys::Rejection::SourceCapReached),
            "{transport:?}: {r:?}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn a_p2p_source_without_a_stream_fails_safely() {
    let pool = live_pool().await;
    let o = origin(2);
    let shard = shard_id(&o);
    let s = Served {
        client: crate::node_http::NodeClient::new(),
        source: crate::node_http::p2p_base_url(&libp2p::PeerId::random()),
        _http: None,
    };
    let r = head_for(&s, &pool, &shard, &Default::default()).await;
    assert!(matches!(r, Err(MirrorWatcherError::Http(_))), "{r:?}");
    assert_eq!(stored(&pool, &shard).await, 0);
}

#[tokio::test]
async fn a_core_shard_over_a_stream_is_verified_against_the_trust_anchor() {
    let anchor_key = SigningKey::from_bytes(&rand::random());
    let make = |signer: &SigningKey| {
        let o = origin(1);
        {
            let mut g = o.lock().unwrap();
            g.shard_id = "core".into();
            g.signing = signer.clone();
        }
        o
    };
    let anchors = vec![avalon_protocol::network_trust::TrustAnchorEntry {
        label: NETWORK.into(),
        network_id: NETWORK.into(),
        verify_key: hex::encode(anchor_key.verifying_key().to_bytes()),
        signing_key_id: "k".into(),
        server_url: None,
        environment: avalon_protocol::network_trust::NetworkEnvironment::Dev,
        seed_nodes: vec![],
        notes: None,
    }];
    let pool = dead_pool();
    let bounds = Default::default();
    let signed = serve(&make(&anchor_key), Transport::Stream).await;
    let got = fetch_and_verify_configured_head(
        &signed.client,
        &pool,
        NETWORK,
        &anchors,
        "core",
        &signed.source,
        &bounds,
        &mut 0,
    )
    .await
    .unwrap();
    assert_eq!(got.unwrap().1, anchor_key.verifying_key());

    let other = SigningKey::from_bytes(&rand::random());
    let forged = serve(&make(&other), Transport::Stream).await;
    let r = fetch_and_verify_configured_head(
        &forged.client,
        &pool,
        NETWORK,
        &anchors,
        "core",
        &forged.source,
        &bounds,
        &mut 0,
    )
    .await;
    assert!(matches!(r, Err(MirrorWatcherError::InvalidSignature)));
}

#[test]
fn unreachable_sources_are_throttled_and_refusals_are_not() {
    // Exercises both arms without asserting log text.
    let unreachable =
        MirrorWatcherError::Http(crate::node_http::NodeHttpError::Invalid("x".into()));
    log_source_failure("p2p://x", &unreachable);
    log_source_failure("p2p://x", &MirrorWatcherError::InvalidSignature);
}
