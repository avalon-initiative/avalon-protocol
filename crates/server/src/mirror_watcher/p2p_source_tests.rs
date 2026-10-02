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
    /// Tree size the head advertises, when not the full ledger.
    advertised: Option<usize>,
    /// A seq the entries listing leaves out.
    gap: Option<usize>,
    /// Serve the proof of the next leaf instead of the one asked for.
    wrong_proof: bool,
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
        advertised: None,
        gap: None,
        wrong_proof: false,
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
            "/ledger/sth/latest" => (200, self.head(self.advertised.unwrap_or(n))),
            "/ledger/entries" => {
                let since: usize = q("since_seq").and_then(|v| v.parse().ok()).unwrap_or(0);
                let limit: usize = q("limit").and_then(|v| v.parse().ok()).unwrap_or(100);
                let page: Vec<_> = (since + 1..=n)
                    .filter(|seq| Some(*seq) != self.gap)
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
                let idx = if self.wrong_proof {
                    seq % size
                } else {
                    seq - 1
                };
                let proof = merkle::inclusion_proof_of_hex_hashes(idx, prefix).unwrap();
                (
                    200,
                    serde_json::json!({
                        "root_hash": hex::encode(merkle::mth_of_hex_hashes(prefix).unwrap()),
                        "leaf_hash": self.hashes[idx],
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
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap()
}

async fn head_for(
    s: &Served,
    pool: &PgPool,
    shard: &str,
    bounds: &crate::self_certifying_keys::MirrorBounds,
) -> Result<Option<(CosignedTreeHead, VerifyingKey)>, MirrorWatcherError> {
    head_for_with(s, pool, NETWORK, shard, bounds, &mut BTreeSet::new()).await
}

async fn head_for_with(
    s: &Served,
    pool: &PgPool,
    network: &str,
    shard: &str,
    bounds: &crate::self_certifying_keys::MirrorBounds,
    admitted_new: &mut BTreeSet<String>,
) -> Result<Option<(CosignedTreeHead, VerifyingKey)>, MirrorWatcherError> {
    let fetched = fetch_source_head(&s.client, &s.source, shard).await?;
    verify_configured_head(
        pool,
        network,
        &[],
        shard,
        &s.source,
        fetched,
        bounds,
        admitted_new,
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
                advertised: None,
                gap: None,
                wrong_proof: false,
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
    let verify = |served: &Served| {
        let (client, source) = (served.client.clone(), served.source.clone());
        let (pool, anchors, bounds) = (pool.clone(), anchors.clone(), bounds);
        async move {
            let fetched = fetch_source_head(&client, &source, "core").await?;
            verify_configured_head(
                &pool,
                NETWORK,
                &anchors,
                "core",
                &source,
                fetched,
                &bounds,
                &mut BTreeSet::new(),
            )
            .await
        }
    };
    let got = verify(&signed).await.unwrap();
    assert_eq!(got.unwrap().1, anchor_key.verifying_key());

    let other = SigningKey::from_bytes(&rand::random());
    let forged = serve(&make(&other), Transport::Stream).await;
    // The head also presents its own key; for a core shard only the anchor counts.
    let r = verify(&forged).await;
    assert!(matches!(r, Err(MirrorWatcherError::InvalidSignature)));
}

fn http_err(kind: crate::node_http::StreamErrorKind) -> MirrorWatcherError {
    MirrorWatcherError::Http(crate::node_http::NodeHttpError::Stream {
        kind,
        message: "x".into(),
    })
}

#[test]
fn only_unreachable_node_shard_sources_are_throttled() {
    use crate::node_http::StreamErrorKind::*;
    let node = &avalon_protocol::shard_identity::derive_self_certifying_id(
        &SigningKey::from_bytes(&[1; 32]).verifying_key(),
    );
    for kind in [Unavailable, Connect, Local, Dropped, Timeout] {
        assert!(failure_is_throttled(node, &http_err(kind)), "{kind:?}");
        // A core or trust-anchor source stays an error every tick.
        assert!(!failure_is_throttled("core", &http_err(kind)), "{kind:?}");
    }
    assert!(!failure_is_throttled(node, &http_err(Protocol)));
    assert!(!failure_is_throttled(
        node,
        &MirrorWatcherError::InvalidSignature
    ));
    assert!(!failure_is_throttled(
        node,
        &MirrorWatcherError::SelfCertifyingRejected(
            crate::self_certifying_keys::Rejection::BadSignature
        )
    ));
    assert!(!failure_is_throttled(
        node,
        &MirrorWatcherError::Http(crate::node_http::NodeHttpError::Invalid("x".into()))
    ));
}

#[test]
fn a_hostile_retry_after_is_clamped() {
    assert_eq!(retry_wait(Some("86400")), MAX_RETRY_AFTER);
    assert_eq!(retry_wait(Some("18446744073709551615")), MAX_RETRY_AFTER);
    assert_eq!(retry_wait(Some("0")), MIN_RETRY_AFTER);
    assert_eq!(retry_wait(Some("junk")), MIN_RETRY_AFTER);
    assert_eq!(retry_wait(None), MIN_RETRY_AFTER);
    assert_eq!(retry_wait(Some("5")), Duration::from_secs(5));
}

#[tokio::test(start_paused = true)]
async fn a_silent_source_is_cut_off_at_the_fetch_deadline() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    // Accepts the request and never answers.
    let held = tokio::spawn(async move {
        let mut keep = Vec::new();
        while let Some(c) = rx.recv().await {
            keep.push(c);
        }
    });
    let client = crate::node_http::NodeClient::new().with_stream(crate::node_http::StreamHandle {
        commands: tx,
        peers: None,
        settings: crate::node_http::NodeHttpSettings::default(),
    });
    let source = crate::node_http::p2p_base_url(&libp2p::PeerId::random());
    let started = tokio::time::Instant::now();
    let r = fetch_source_head(&client, &source, "node:x").await;
    assert!(
        matches!(&r, Err(e) if is_unreachable(e)),
        "{:?}",
        r.err().map(|e| e.to_string())
    );
    assert!(started.elapsed() <= SOURCE_FETCH_DEADLINE + Duration::from_secs(1));
    held.abort();
}

#[tokio::test]
async fn a_node_shard_presented_with_the_anchor_key_is_refused() {
    // The head is signed by the trust-anchor key, but the shard id hashes another key.
    let anchor_key = SigningKey::from_bytes(&rand::random());
    let o = origin(1);
    o.lock().unwrap().signing = anchor_key;
    let shard = shard_id(&o);
    let s = serve(&o, Transport::Stream).await;
    let r = head_for(&s, &dead_pool(), &shard, &Default::default()).await;
    assert!(
        is_rejection(
            &r,
            crate::self_certifying_keys::Rejection::KeyDoesNotMatchId
        ),
        "{r:?}"
    );
}

#[tokio::test]
async fn a_head_for_another_network_is_refused_on_either_transport() {
    for transport in BOTH {
        let o = origin(1);
        let shard = shard_id(&o);
        let s = serve(&o, transport).await;
        let r = head_for_with(
            &s,
            &dead_pool(),
            "some-other-network",
            &shard,
            &Default::default(),
            &mut BTreeSet::new(),
        )
        .await;
        assert!(
            is_rejection(&r, crate::self_certifying_keys::Rejection::WrongNetwork),
            "{transport:?}: {r:?}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn the_new_shard_budget_is_shared_charged_on_success_and_never_double_counted() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds {
        max_new_per_tick: 2,
        ..Default::default()
    };
    let mut admitted = BTreeSet::new();
    // An offline source and a refused head spend nothing.
    let offline = Served {
        client: crate::node_http::NodeClient::new(),
        source: crate::node_http::p2p_base_url(&libp2p::PeerId::random()),
        _http: None,
    };
    for _ in 0..4 {
        let o = origin(1);
        let r = head_for_with(
            &offline,
            &pool,
            NETWORK,
            &shard_id(&o),
            &bounds,
            &mut admitted,
        )
        .await;
        assert!(r.is_err());
    }
    assert!(admitted.is_empty());

    let (a, b, c) = (origin(1), origin(1), origin(1));
    let on_stream = serve(&a, Transport::Stream).await;
    let on_http = serve(&a, Transport::Http).await;
    for served in [&on_stream, &on_http] {
        // The same shard from a second source is one charge, not two.
        let got = head_for_with(
            served,
            &pool,
            NETWORK,
            &shard_id(&a),
            &bounds,
            &mut admitted,
        )
        .await
        .unwrap();
        assert!(got.is_some());
    }
    assert_eq!(admitted.len(), 1);
    let sb = serve(&b, Transport::Stream).await;
    assert!(
        head_for_with(&sb, &pool, NETWORK, &shard_id(&b), &bounds, &mut admitted)
            .await
            .unwrap()
            .is_some()
    );
    // The budget of 2 is spent: a third new shard is skipped (Ok(None)), stores nothing.
    let sc = serve(&c, Transport::Stream).await;
    assert!(
        head_for_with(&sc, &pool, NETWORK, &shard_id(&c), &bounds, &mut admitted)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!admitted.contains(&shard_id(&c)));
    assert_eq!(stored(&pool, &shard_id(&c)).await, 0);
    // Starting from a non-zero count (a prior charge) leaves one slot.
    let mut prior = BTreeSet::from(["node:someone-else".to_string()]);
    let d = origin(1);
    let sd = serve(&d, Transport::Stream).await;
    assert!(
        head_for_with(&sd, &pool, NETWORK, &shard_id(&d), &bounds, &mut prior)
            .await
            .unwrap()
            .is_some()
    );
    let e = origin(1);
    let se = serve(&e, Transport::Stream).await;
    assert!(
        head_for_with(&se, &pool, NETWORK, &shard_id(&e), &bounds, &mut prior)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
#[ignore]
async fn a_smaller_or_repeated_head_is_not_a_new_head_nor_an_equivocation() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(5);
        let shard = shard_id(&o);
        let s = serve(&o, transport).await;
        let chain = PostgresSettlementProvider::new(pool.clone(), NETWORK.to_string());
        let observe = |head: CosignedTreeHead| {
            let (pool, chain, shard, source) =
                (pool.clone(), chain.clone(), shard.clone(), s.source.clone());
            async move {
                let observed =
                    ObservedSth::from_sth(&source, &shard, &head.sth, OffsetDateTime::now_utc());
                let is_new = mirror::insert_observation(&pool, &observed).await.unwrap();
                if is_new {
                    check_equivocation(&pool, &chain, "core", &observed)
                        .await
                        .unwrap();
                }
                is_new
            }
        };
        let (full, _) = head_for(&s, &pool, &shard, &bounds).await.unwrap().unwrap();
        assert!(observe(full.clone()).await);
        backfill_once(&s, &pool, &shard, &full, limits(100))
            .await
            .unwrap();
        assert_eq!(stored(&pool, &shard).await, 5);
        // The same head again is a repeat: nothing new is observed.
        assert!(!observe(full.clone()).await);
        // A smaller, consistent head is a new observation but never an equivocation (they are
        // compared per tree size), and backfill, already past it, stores nothing more.
        o.lock().unwrap().advertised = Some(3);
        let (small, _) = head_for(&s, &pool, &shard, &bounds).await.unwrap().unwrap();
        assert_eq!(small.sth.tree_size, 3);
        assert!(observe(small.clone()).await);
        assert!(mirror::unresolved_equivocations(&pool, NETWORK, &shard)
            .await
            .unwrap()
            .is_empty());
        backfill_once(&s, &pool, &shard, &small, limits(100))
            .await
            .unwrap();
        assert_eq!(stored(&pool, &shard).await, 5, "{transport:?}");
    }
}

#[tokio::test]
#[ignore]
async fn entries_with_a_gap_are_refused_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(4);
        let shard = shard_id(&o);
        o.lock().unwrap().gap = Some(2);
        let s = serve(&o, transport).await;
        let (head, _) = head_for(&s, &pool, &shard, &bounds).await.unwrap().unwrap();
        let r = backfill_once(&s, &pool, &shard, &head, limits(100)).await;
        assert!(
            matches!(
                r,
                Err(MirrorWatcherError::InvalidInclusionProof { seq: 3, .. })
            ),
            "{transport:?}: {r:?}"
        );
        // Only the entry before the gap was stored.
        assert_eq!(stored(&pool, &shard).await, 1, "{transport:?}");
    }
}

#[tokio::test]
#[ignore]
async fn a_proof_for_a_different_leaf_is_refused_on_either_transport() {
    let pool = live_pool().await;
    let bounds = crate::self_certifying_keys::MirrorBounds::default();
    for transport in BOTH {
        let o = origin(3);
        let shard = shard_id(&o);
        o.lock().unwrap().wrong_proof = true;
        let s = serve(&o, transport).await;
        let (head, _) = head_for(&s, &pool, &shard, &bounds).await.unwrap().unwrap();
        let r = backfill_once(&s, &pool, &shard, &head, limits(100)).await;
        assert!(
            matches!(
                r,
                Err(MirrorWatcherError::InvalidInclusionProof { seq: 1, .. })
            ),
            "{transport:?}: {r:?}"
        );
        assert_eq!(stored(&pool, &shard).await, 0, "{transport:?}");
    }
}
