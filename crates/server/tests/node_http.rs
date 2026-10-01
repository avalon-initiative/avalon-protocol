//! Node-to-node HTTP over libp2p streams between real in-process swarms on loopback, served by a
//! small axum router mounted as the node's router. No database needed.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use avalon_protocol::connectivity::Connectivity;
use avalon_protocol::node_request::{
    encode_node_request_header, sign_node_request, NodeRequestTarget,
};
use avalon_server::dht::{self, DhtConfig, DhtHandle};
use avalon_server::node_auth::{require_node_auth, AuthenticatedNode, NodeAuth, CREDENTIAL_PATHS};
use avalon_server::node_http::{
    p2p_base_url, synthetic_addr, NodeClient, NodeHttpError, NodeHttpSettings, NodeSigner,
    RemotePeer, StreamErrorKind, StreamHandle,
};
use avalon_server::nodes::{PeerInfo, PeerTable};
use avalon_server::reachability::{AutonatSettings, ReachabilitySnapshot};
use avalon_server::relay::{RelayClientSettings, RelayServerSettings, RelaySettings};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query};
use axum::http::{Method, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use libp2p::{identity, Multiaddr, PeerId};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(30);
const NETWORK: &str = "avalon-test";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(listen: &str, external: Option<&str>, relay: RelaySettings) -> DhtConfig {
    DhtConfig {
        identity: identity::Keypair::generate_ed25519(),
        network_id: NETWORK.to_string(),
        listen_addr: listen.parse().unwrap(),
        external_addr: external.map(|e| e.parse().unwrap()),
        autonat: AutonatSettings {
            allow_private: true,
            boot_delay: Duration::from_millis(100),
            retry_interval: Duration::from_millis(300),
            throttle_server_period: Duration::ZERO,
            ..AutonatSettings::default()
        },
        relay,
        bootstrap_scan_interval: Duration::from_millis(300),
    }
}

/// Counters and gates the test handlers share with the test body.
#[derive(Clone, Default)]
struct Probe {
    /// Requests that reached a handler.
    hits: Arc<AtomicUsize>,
    /// Requests parked in the blocking handler.
    parked: Arc<AtomicUsize>,
    release: Arc<Notify>,
}

fn test_router(p: Probe) -> Router {
    let hits = p.hits.clone();
    let count = move || hits.fetch_add(1, Ordering::SeqCst);
    let (c1, c2, c3, c4, c5, c6) = (
        count.clone(),
        count.clone(),
        count.clone(),
        count.clone(),
        count.clone(),
        count,
    );
    let parked = p.parked.clone();
    let release = p.release.clone();
    Router::new()
        .route(
            "/nodes/status",
            get(move || async move {
                c1();
                (
                    [("x-test", "yes"), ("content-type", "application/json")],
                    r#"{"ok":true}"#,
                )
            }),
        )
        .route(
            "/nodes/announce",
            post(
                move |headers: axum::http::HeaderMap, body: Bytes| async move {
                    c2();
                    let ct = headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    (
                        StatusCode::CREATED,
                        [("x-test", "posted".to_string()), ("x-echo-ct", ct)],
                        body,
                    )
                },
            ),
        )
        .route(
            "/ledger/sth/latest",
            get(move |Query(q): Query<HashMap<String, String>>| async move {
                c3();
                Json(q)
            }),
        )
        .route(
            "/nodes/peers",
            get(
                move |peer: Option<Extension<RemotePeer>>,
                      ConnectInfo(addr): ConnectInfo<SocketAddr>| async move {
                    c4();
                    Json(serde_json::json!({
                        "peer": peer.map(|Extension(RemotePeer(p))| p.to_string()),
                        "addr": addr.to_string(),
                    }))
                },
            ),
        )
        .route(
            "/nodes/relay",
            post(move || async move {
                c5();
                vec![7u8; 100 * 1024]
            }),
        )
        .route(
            "/nodes/trace",
            post(move || {
                let (parked, release) = (parked.clone(), release.clone());
                async move {
                    parked.fetch_add(1, Ordering::SeqCst);
                    release.notified().await;
                    "released"
                }
            }),
        )
        .route(
            "/auth/login",
            get(move || async move {
                c6();
                "user route"
            }),
        )
}

struct Node {
    handle: DhtHandle,
    peers: PeerTable,
}

impl Node {
    fn client(&self) -> NodeClient {
        NodeClient::new().with_stream(self.handle.node_http.clone())
    }

    fn url(&self) -> String {
        p2p_base_url(&self.handle.peer_id)
    }
}

fn info_for(node: &DhtHandle, addrs: Vec<String>, connectivity: Option<Connectivity>) -> PeerInfo {
    PeerInfo {
        identity_bound: true,
        base_url: format!("http://{}.test", node.peer_id),
        roles: vec!["combined".to_string()],
        protocol_version: avalon_server::version::PROTOCOL_VERSION.to_string(),
        network_id: NETWORK.to_string(),
        last_announced_at: time::OffsetDateTime::now_utc(),
        libp2p_peer_id: Some(node.peer_id.to_string()),
        libp2p_listen_addrs: addrs,
        connectivity,
        witness: None,
    }
}

async fn direct_node(settings: NodeHttpSettings) -> Node {
    direct_node_with(identity::Keypair::generate_ed25519(), settings).await
}

async fn direct_node_with(key: identity::Keypair, settings: NodeHttpSettings) -> Node {
    let peers = PeerTable::new();
    let addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let handle = dht::start_with_node_http(
        peers.clone(),
        DhtConfig {
            identity: key,
            ..config(&addr, None, RelaySettings::default())
        },
        settings,
    )
    .await;
    Node { handle, peers }
}

/// `to` is introduced to `from`'s peer table at its direct address.
fn introduce(from: &Node, to: &Node) {
    let addrs = to
        .handle
        .listen_addrs
        .iter()
        .map(|a| a.to_string())
        .collect();
    from.peers.upsert(info_for(&to.handle, addrs, None));
}

async fn served(probe: &Probe, settings: NodeHttpSettings) -> (Node, Node) {
    let a = direct_node(NodeHttpSettings::default()).await;
    let b = direct_node(settings).await;
    b.handle.router_slot.set(test_router(probe.clone()));
    introduce(&a, &b);
    (a, b)
}

async fn get_ok(client: &NodeClient, url: String) -> (StatusCode, String) {
    let res = client.get(url).send().await.expect("stream request");
    (res.status(), res.text().await.unwrap())
}

#[tokio::test]
async fn get_and_post_round_trip_and_match_http() {
    let probe = Probe::default();
    let (a, b) = served(&probe, NodeHttpSettings::default()).await;
    let client = a.client();

    // The same router over plain HTTP, for parity.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_base = format!("http://{}", listener.local_addr().unwrap());
    let http_router = test_router(probe.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            http_router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    for base in [b.url(), http_base.clone()] {
        let res = client
            .get(format!("{base}/nodes/status"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get("x-test").unwrap(), "yes");
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(res.text().await.unwrap(), r#"{"ok":true}"#);

        let res = client
            .post(format!("{base}/nodes/announce"))
            .json(&serde_json::json!({"hello": "world"}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        assert_eq!(res.headers().get("x-test").unwrap(), "posted");
        assert_eq!(res.headers().get("x-echo-ct").unwrap(), "application/json");
        assert_eq!(res.text().await.unwrap(), r#"{"hello":"world"}"#);

        let res = client
            .get(format!("{base}/ledger/sth/latest"))
            .query(&[("shard_id", "core"), ("witnesses", "1")])
            .send()
            .await
            .unwrap();
        let q: HashMap<String, String> = res.json().await.unwrap();
        assert_eq!(q["shard_id"], "core");
        assert_eq!(q["witnesses"], "1");
    }
}

#[tokio::test]
async fn the_handler_sees_the_authenticated_peer_and_a_bounded_set_of_addresses() {
    let probe = Probe::default();
    let (a, b) = served(&probe, NodeHttpSettings::default()).await;
    let a2 = direct_node(NodeHttpSettings::default()).await;
    introduce(&a2, &b);

    let seen = |client: NodeClient| {
        let url = format!("{}/nodes/peers", b.url());
        async move {
            let res = client.get(url).send().await.unwrap();
            res.json::<serde_json::Value>().await.unwrap()
        }
    };
    // Neither is bound on b: the handler still sees who they are, but on one shared address.
    let one = seen(a.client()).await;
    let two = seen(a2.client()).await;
    assert_eq!(one["peer"], a.handle.peer_id.to_string());
    assert_eq!(two["peer"], a2.handle.peer_id.to_string());
    assert_eq!(
        one["addr"],
        avalon_server::node_http::SHARED_PEER_ADDR.to_string()
    );
    assert_eq!(one["addr"], two["addr"]);

    // Once b binds a, a gets its own stable address; a2 still shares.
    introduce(&b, &a);
    let bound = seen(a.client()).await;
    assert_eq!(
        bound["addr"],
        synthetic_addr(&a.handle.peer_id, true).to_string()
    );
    assert_ne!(bound["addr"], one["addr"]);
    assert!(!bound["addr"].as_str().unwrap().starts_with("127."));
    assert_eq!(seen(a2.client()).await["addr"], one["addr"]);
}

/// The three credential routes behind the real middleware, counting handler hits.
fn auth_router(auth: &NodeAuth, hits: Arc<AtomicUsize>) -> Router {
    let mut router = Router::new();
    for path in CREDENTIAL_PATHS {
        let hits = hits.clone();
        router = router.route(
            path,
            post(move |node: AuthenticatedNode| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                node.0.to_string()
            })
            .layer(axum::middleware::from_fn_with_state(
                auth.route(1024),
                require_node_auth,
            )),
        );
    }
    router
}

/// `node` serving `auth_router` over its stream and, on a loopback port, over plain HTTP.
async fn serve_credential_routes(node: &Node, hits: Arc<AtomicUsize>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_base = format!("http://{}", listener.local_addr().unwrap());
    let auth = NodeAuth::new(
        node.peers.clone(),
        NETWORK,
        Some(&node.handle.peer_id.to_string()),
        Some(&http_base),
    );
    let router = auth_router(&auth, hits).route("/nodes/status", get(|| async { "ok" }));
    node.handle.router_slot.set(router.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    http_base
}

#[tokio::test]
async fn write_routes_over_a_stream_follow_standing() {
    let hits = Arc::new(AtomicUsize::new(0));
    let a = direct_node(NodeHttpSettings::default()).await;
    let b = direct_node(NodeHttpSettings::default()).await;
    serve_credential_routes(&b, hits.clone()).await;
    introduce(&a, &b);
    let client = a.client();
    for path in CREDENTIAL_PATHS {
        let res = client
            .post(format!("{}{path}", b.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    // Open routes still work for the same unknown peer.
    let (status, _) = get_ok(&client, format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::OK);

    // Standing through a self-announced p2p:// entry, which has no public URL at all.
    b.peers.upsert(PeerInfo {
        base_url: p2p_base_url(&a.handle.peer_id),
        ..info_for(&a.handle, vec![], None)
    });
    for path in CREDENTIAL_PATHS {
        let res = client
            .post(format!("{}{path}", b.url()))
            .header("x-avalon-node-auth", "v1; garbage")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        assert_eq!(res.text().await.unwrap(), a.handle.peer_id.to_string());
    }
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

fn header_with(
    key: &identity::Keypair,
    network: &str,
    recipient: &str,
    ts: i64,
    nonce: [u8; 16],
    body: &[u8],
) -> String {
    let ed = key.clone().try_into_ed25519().unwrap();
    let seed: [u8; 32] = ed.secret().as_ref().try_into().unwrap();
    let target = NodeRequestTarget {
        method: "POST",
        path: "/nodes/relay",
        body,
        network_id: network,
    };
    let peer = PeerId::from(key.public()).to_string();
    let auth = sign_node_request(
        &ed25519_dalek::SigningKey::from_bytes(&seed),
        &peer,
        &target,
        recipient,
        ts,
        nonce,
    )
    .unwrap();
    encode_node_request_header(&auth)
}

#[tokio::test]
async fn write_routes_over_http_need_a_valid_unreplayed_credential_from_a_node_with_standing() {
    let hits = Arc::new(AtomicUsize::new(0));
    let a_key = identity::Keypair::generate_ed25519();
    let b = direct_node(NodeHttpSettings::default()).await;
    let http_base = serve_credential_routes(&b, hits.clone()).await;
    let a_id = PeerId::from(a_key.public());
    b.peers.upsert(PeerInfo {
        base_url: format!("http://{a_id}.test"),
        libp2p_peer_id: Some(a_id.to_string()),
        ..info_for(&b.handle, vec![], None)
    });
    let b_id = b.handle.peer_id.to_string();
    let url = format!("{http_base}/nodes/relay");
    let http = reqwest::Client::new();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let send = |header: Option<String>, body: &'static str| {
        let mut req = http.post(&url).body(body);
        if let Some(h) = header {
            req = req.header("x-avalon-node-auth", h);
        }
        async move { req.send().await.unwrap().status() }
    };
    let h = |net: &str, rcpt: &str, ts: i64, nonce: u8, body: &[u8]| {
        header_with(&a_key, net, rcpt, ts, [nonce; 16], body)
    };
    let unauthorized = StatusCode::UNAUTHORIZED;

    assert_eq!(send(None, "{}").await, unauthorized);
    assert_eq!(send(Some("garbage".into()), "{}").await, unauthorized);
    let stale = h(NETWORK, &b_id, now - 3600, 1, b"{}");
    assert_eq!(send(Some(stale), "{}").await, unauthorized);
    let future = h(NETWORK, &b_id, now + 3600, 2, b"{}");
    assert_eq!(send(Some(future), "{}").await, unauthorized);
    let wrong_rcpt = h(NETWORK, "http://elsewhere.test", now, 3, b"{}");
    assert_eq!(send(Some(wrong_rcpt), "{}").await, unauthorized);
    let wrong_net = h("other-net", &b_id, now, 4, b"{}");
    assert_eq!(send(Some(wrong_net), "{}").await, unauthorized);
    let mut bad_sig = h(NETWORK, &b_id, now, 5, b"{}");
    bad_sig.replace_range(
        bad_sig.len() - 1..,
        if bad_sig.ends_with('0') { "1" } else { "0" },
    );
    assert_eq!(send(Some(bad_sig), "{}").await, unauthorized);
    let other_body = h(NETWORK, &b_id, now, 6, b"{}");
    assert_eq!(send(Some(other_body), "{ }").await, unauthorized);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // A key nobody announced is refused whatever it signs.
    let stranger = identity::Keypair::generate_ed25519();
    let own = header_with(&stranger, NETWORK, &b_id, now, [7; 16], b"{}");
    assert_eq!(send(Some(own), "{}").await, StatusCode::FORBIDDEN);

    // The real signer's requests go through, by peer id and by URL, once each.
    let good = h(NETWORK, &b_id, now, 8, b"{}");
    assert_eq!(send(Some(good.clone()), "{}").await, StatusCode::OK);
    assert_eq!(send(Some(good), "{}").await, unauthorized);
    let by_url = h(NETWORK, &http_base, now, 9, b"{}");
    assert_eq!(send(Some(by_url), "{}").await, StatusCode::OK);
    let signer = NodeSigner::new(&a_key, NETWORK).unwrap();
    let client = NodeClient::new().with_signer(signer);
    for _ in 0..2 {
        let res = client.post(&url).body("{}").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await.unwrap(), a_id.to_string());
    }
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_node_reaches_the_write_routes_over_http_signed_and_over_the_stream_unsigned() {
    let hits = Arc::new(AtomicUsize::new(0));
    let a_key = identity::Keypair::generate_ed25519();
    let a = direct_node_with(a_key.clone(), NodeHttpSettings::default()).await;
    let b = direct_node(NodeHttpSettings::default()).await;
    let http_base = serve_credential_routes(&b, hits.clone()).await;
    // b gives a standing; a knows b at its live URL.
    introduce(&b, &a);
    a.peers.upsert(PeerInfo {
        base_url: http_base.clone(),
        ..info_for(
            &b.handle,
            b.handle
                .listen_addrs
                .iter()
                .map(|x| x.to_string())
                .collect(),
            None,
        )
    });
    let client = NodeClient::new()
        .with_stream(a.handle.node_http.clone())
        .with_signer(NodeSigner::new(&a_key, NETWORK).unwrap());

    let res = client
        .post(format!("{http_base}/mirror/notify"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(!res.via_stream());

    // A p2p:// URL goes over the stream, where the handshake is the credential.
    let res = client
        .post(format!("{}/nodes/replicate-chat", b.url()))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.via_stream());
    assert_eq!(res.text().await.unwrap(), a.handle.peer_id.to_string());

    // The URL goes down: the retry over the stream still authenticates.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_base = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let dead_table = PeerTable::new();
    dead_table.upsert(PeerInfo {
        base_url: dead_base.clone(),
        ..info_for(
            &b.handle,
            b.handle
                .listen_addrs
                .iter()
                .map(|x| x.to_string())
                .collect(),
            None,
        )
    });
    let client = client.with_stream(StreamHandle {
        peers: Some(dead_table),
        ..a.handle.node_http.clone()
    });
    let res = client
        .post(format!("{dead_base}/nodes/relay"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.via_stream());
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    // Without a signer the same HTTP request is refused.
    let unsigned = NodeClient::new()
        .post(format!("{http_base}/mirror/notify"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_buffer_budget_answers_429_without_reading_a_body_it_cannot_hold() {
    let probe = Probe::default();
    let (a, b) = served(
        &probe,
        NodeHttpSettings {
            max_request_bytes: 1024,
            buffer_budget_bytes: 1024,
            max_inflight: 10,
            max_inflight_per_peer: 10,
            ..NodeHttpSettings::default()
        },
    )
    .await;
    let client = a.client();
    let parked = {
        let (client, url) = (client.clone(), format!("{}/nodes/trace", b.url()));
        let task = tokio::spawn(async move {
            client
                .post(url)
                .body(vec![1u8; 1000])
                .send()
                .await
                .unwrap()
                .status()
        });
        tokio::time::timeout(WAIT, async {
            while probe.parked.load(Ordering::SeqCst) < 1 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        task
    };
    // The first body holds the whole budget while its handler runs.
    let res = client
        .post(format!("{}/nodes/announce", b.url()))
        .body(vec![2u8; 1000])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(probe.hits.load(Ordering::SeqCst), 0);

    probe.release.notify_waiters();
    assert_eq!(parked.await.unwrap(), StatusCode::OK);
    let res = client
        .post(format!("{}/nodes/announce", b.url()))
        .body(vec![2u8; 1000])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn the_connection_limit_refuses_a_second_peer() {
    let probe = Probe::default();
    let (a, b) = served(
        &probe,
        NodeHttpSettings {
            max_connections: 1,
            ..NodeHttpSettings::default()
        },
    )
    .await;
    let (status, _) = get_ok(&a.client(), format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::OK);

    let other = direct_node(NodeHttpSettings::default()).await;
    introduce(&other, &b);
    let err = other
        .client()
        .get(format!("{}/nodes/status", b.url()))
        .send()
        .await
        .err()
        .expect("b is at its connection limit");
    // A refusal by the limit looks like a dropped connection, which never fails over to HTTP.
    assert!(
        matches!(
            err,
            NodeHttpError::Stream {
                kind: StreamErrorKind::Dropped,
                ..
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn no_more_than_the_stream_limit_run_at_once() {
    let probe = Probe::default();
    let (a, b) = served(
        &probe,
        NodeHttpSettings {
            max_inflight: 64,
            max_inflight_per_peer: 64,
            ..NodeHttpSettings::default()
        },
    )
    .await;
    let client = a.client();
    let tasks: Vec<_> = (0..24)
        .map(|_| {
            let (client, url) = (client.clone(), format!("{}/nodes/trace", b.url()));
            tokio::spawn(async move {
                client
                    .post(url)
                    .timeout(Duration::from_secs(4))
                    .send()
                    .await
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let running = probe.parked.load(Ordering::SeqCst);
    assert!(running <= 16, "{running} handlers ran at once");
    probe.release.notify_waiters();
    for t in tasks {
        let _ = t.await;
    }
}

#[tokio::test]
async fn a_peer_reachable_only_through_a_relay_answers_over_the_stream() {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        return eprintln!("skipping: no IPv6 loopback");
    }
    // The relay, dialable on IPv4 loopback.
    let relay_addr = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let relay = dht::start(
        PeerTable::new(),
        config(
            &relay_addr,
            Some(&relay_addr),
            RelaySettings {
                server: Some(RelayServerSettings::default()),
                ..RelaySettings::default()
            },
        ),
    )
    .await;
    let relay_full: Multiaddr = format!("{relay_addr}/p2p/{}", relay.peer_id)
        .parse()
        .unwrap();

    // The server listens on IPv6 loopback only, so the IPv4 client has no direct path to it.
    let server_peers = PeerTable::new();
    let server = dht::start(
        server_peers.clone(),
        config(
            "/ip6/::1/tcp/0",
            None,
            RelaySettings {
                client: RelayClientSettings {
                    max_reservations: 1,
                    relay_addrs: vec![relay_full],
                    allow_private: true,
                    retry_backoff: Duration::from_secs(60),
                    pending_timeout: Duration::from_secs(10),
                    reconcile_interval: Duration::from_millis(200),
                    ..RelayClientSettings::default()
                },
                ..RelaySettings::default()
            },
        ),
    )
    .await;
    server_peers.upsert(info_for(
        &relay,
        relay.listen_addrs.iter().map(|a| a.to_string()).collect(),
        None,
    ));
    let probe = Probe::default();
    server.router_slot.set(test_router(probe.clone()));

    let relayed = tokio::time::timeout(WAIT, async {
        let mut rx = server.reachability.subscribe();
        loop {
            let found = |s: &ReachabilitySnapshot| {
                s.relay_reservations
                    .iter()
                    .find(|r| r.relay_peer_id == relay.peer_id.to_string())
                    .map(|r| r.relayed_addr.clone())
            };
            if let Some(addr) = found(&rx.borrow_and_update()) {
                return addr;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("the private node reserves a slot on the relay");

    let client_peers = PeerTable::new();
    let client_node = dht::start(
        client_peers.clone(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
    )
    .await;
    let mut server_info = info_for(&server, vec![relayed], Some(Connectivity::Relayed));
    server_info.base_url = "http://unreachable.invalid".to_string();
    // Selection sends this peer over the stream, not to its dead URL.
    let url = NodeClient::url_for(&server_info);
    assert_eq!(url, p2p_base_url(&server.peer_id));
    client_peers.upsert(server_info);
    let client = NodeClient::new().with_stream(client_node.node_http.clone());

    let (status, body) = get_ok(&client, format!("{url}/nodes/status")).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, r#"{"ok":true}"#));
    let res = client
        .post(format!("{url}/nodes/announce"))
        .body(Bytes::from_static(b"through the relay"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(res.text().await.unwrap(), "through the relay");
    assert!(relay.relay_stats.counts().circuits_accepted >= 1);

    // With no address for it, the same peer cannot be reached at all.
    let stranger = direct_node(NodeHttpSettings::default()).await;
    let started = Instant::now();
    let err = stranger
        .client()
        .get(format!("{url}/nodes/status"))
        .send()
        .await
        .err()
        .expect("no route to the private peer");
    // Refused locally for want of an address, which says nothing about the peer.
    assert!(err.is_local(), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn disallowed_paths_and_methods_never_reach_a_handler() {
    let probe = Probe::default();
    let (a, b) = served(&probe, NodeHttpSettings::default()).await;
    let client = a.client();
    for path in [
        "/auth/login",
        "/nodes/log-level",
        "/internal/indexer/apply",
        "/nodes/status/../../auth/login",
        "/nodes/%73tatus",
    ] {
        let (status, _) = get_ok(&client, format!("{}{path}", b.url())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    let res = client
        .request(Method::DELETE, format!("{}/nodes/status", b.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(probe.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_request_before_the_router_exists_gets_503_then_works() {
    let a = direct_node(NodeHttpSettings::default()).await;
    let b = direct_node(NodeHttpSettings::default()).await;
    introduce(&a, &b);
    let client = a.client();
    let (status, _) = get_ok(&client, format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    b.handle.router_slot.set(test_router(Probe::default()));
    let (status, _) = get_ok(&client, format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_oversized_request_is_rejected_by_the_receiver_and_by_the_sender() {
    let probe = Probe::default();
    let small = NodeHttpSettings {
        max_request_bytes: 1024,
        ..NodeHttpSettings::default()
    };
    let (a, b) = served(&probe, small).await;
    let client = a.client();

    // The receiver's limit: the sender allows this size, the receiver refuses it while reading.
    let err = client
        .post(format!("{}/nodes/announce", b.url()))
        .body(vec![1u8; 4096])
        .send()
        .await
        .err()
        .expect("receiver refuses the oversized body");
    assert!(matches!(err, NodeHttpError::Stream { .. }), "{err}");
    assert_eq!(probe.hits.load(Ordering::SeqCst), 0);

    // The sender's own limit fires before anything is sent.
    let tiny = direct_node(NodeHttpSettings {
        max_request_bytes: 16,
        ..NodeHttpSettings::default()
    })
    .await;
    introduce(&tiny, &b);
    let err = tiny
        .client()
        .post(format!("{}/nodes/announce", b.url()))
        .body(vec![1u8; 64])
        .send()
        .await
        .err()
        .expect("sender refuses to write");
    assert!(matches!(err, NodeHttpError::Stream { .. }), "{err}");
    assert_eq!(probe.hits.load(Ordering::SeqCst), 0);

    // A body within both limits still goes through.
    let res = client
        .post(format!("{}/nodes/announce", b.url()))
        .body(vec![1u8; 512])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn an_oversized_response_is_rejected_by_the_receiving_client() {
    let probe = Probe::default();
    let a = direct_node(NodeHttpSettings {
        max_response_bytes: 4096,
        ..NodeHttpSettings::default()
    })
    .await;
    let b = direct_node(NodeHttpSettings::default()).await;
    b.handle.router_slot.set(test_router(probe.clone()));
    introduce(&a, &b);
    introduce(&b, &a);
    let client = a.client();

    let err = client
        .post(format!("{}/nodes/relay", b.url()))
        .send()
        .await
        .err()
        .expect("the 100 KiB response exceeds this client's limit");
    assert!(matches!(err, NodeHttpError::Stream { .. }), "{err}");
    assert_eq!(probe.hits.load(Ordering::SeqCst), 1, "the handler did run");

    // A response within the limit is fine.
    let (status, _) = get_ok(&client, format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::OK);
}

/// Parks `n` requests in the blocking handler and returns their join handles.
async fn park(
    client: &NodeClient,
    url: &str,
    probe: &Probe,
    n: usize,
) -> Vec<tokio::task::JoinHandle<StatusCode>> {
    let target = probe.parked.load(Ordering::SeqCst) + n;
    let tasks = (0..n)
        .map(|_| {
            let (client, url) = (client.clone(), format!("{url}/nodes/trace"));
            tokio::spawn(async move { client.post(url).send().await.unwrap().status() })
        })
        .collect();
    tokio::time::timeout(WAIT, async {
        while probe.parked.load(Ordering::SeqCst) < target {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("requests reach the handler");
    tasks
}

#[tokio::test]
async fn the_per_peer_limit_answers_429_at_once() {
    let probe = Probe::default();
    let (a, b) = served(
        &probe,
        NodeHttpSettings {
            max_inflight: 10,
            max_inflight_per_peer: 2,
            ..NodeHttpSettings::default()
        },
    )
    .await;
    let client = a.client();
    let parked = park(&client, &b.url(), &probe, 2).await;

    let started = Instant::now();
    let (status, _) = get_ok(&client, format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "answered without queueing"
    );
    assert_eq!(probe.hits.load(Ordering::SeqCst), 0);

    probe.release.notify_waiters();
    for t in parked {
        assert_eq!(t.await.unwrap(), StatusCode::OK);
    }
    // Slots are released: the same peer is served again.
    let (status, _) = get_ok(&client, format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_total_limit_answers_429_to_a_different_peer() {
    let probe = Probe::default();
    let (a, b) = served(
        &probe,
        NodeHttpSettings {
            max_inflight: 2,
            max_inflight_per_peer: 2,
            ..NodeHttpSettings::default()
        },
    )
    .await;
    let other = direct_node(NodeHttpSettings::default()).await;
    introduce(&other, &b);
    let parked = park(&a.client(), &b.url(), &probe, 2).await;

    let started = Instant::now();
    let (status, _) = get_ok(&other.client(), format!("{}/nodes/status", b.url())).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(started.elapsed() < Duration::from_secs(3));

    probe.release.notify_waiters();
    for t in parked {
        assert_eq!(t.await.unwrap(), StatusCode::OK);
    }
}

#[tokio::test]
async fn timeouts_map_to_errors_and_to_504() {
    let probe = Probe::default();
    let (a, b) = served(
        &probe,
        NodeHttpSettings {
            timeout: Duration::from_millis(400),
            ..NodeHttpSettings::default()
        },
    )
    .await;
    let client = a.client();

    // The caller gives up first.
    let err = client
        .post(format!("{}/nodes/trace", b.url()))
        .timeout(Duration::from_millis(150))
        .send()
        .await
        .err()
        .expect("caller timeout");
    assert!(err.is_timeout(), "{err}");
    assert!(matches!(
        err,
        NodeHttpError::Stream {
            kind: StreamErrorKind::Timeout,
            ..
        }
    ));

    // The receiver's own bound answers 504 for a handler that never finishes.
    let res = client
        .post(format!("{}/nodes/trace", b.url()))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn an_http_request_to_a_dead_url_falls_back_to_the_stream() {
    let probe = Probe::default();
    let (a, b) = served(&probe, NodeHttpSettings::default()).await;
    // The peer table maps b's dead URL to its libp2p id, as an announce would have.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_url = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let mut info = info_for(
        &b.handle,
        b.handle
            .listen_addrs
            .iter()
            .map(|x| x.to_string())
            .collect(),
        None,
    );
    info.base_url = dead_url.clone();
    // Replace the entry `served` introduced: an id held by two entries is not failed over.
    a.peers
        .prune_older_than(time::OffsetDateTime::now_utc() + time::Duration::hours(1));
    a.peers.upsert(info);

    let (status, body) = get_ok(&a.client(), format!("{dead_url}/nodes/status")).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, r#"{"ok":true}"#));
    assert_eq!(probe.hits.load(Ordering::SeqCst), 1);
    let _: PeerId = b.handle.peer_id;
}
