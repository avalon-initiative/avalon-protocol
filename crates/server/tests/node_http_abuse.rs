//! Hostile peers on the node-to-node stream protocol: a raw libp2p stream peer writes garbage,
//! truncated and oversized frames and floods requests at a real node, which must refuse them
//! without running a handler and keep serving an honest peer. No database needed.

mod abuse_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use abuse_support::*;
use avalon_server::dht::{self, DhtHandle};
use avalon_server::node_http::{p2p_base_url, protocol_name, NodeClient, NodeHttpSettings};
use avalon_server::nodes::PeerTable;
use avalon_server::relay::RelaySettings;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use libp2p::Multiaddr;

struct Served {
    node: DhtHandle,
    addr: Multiaddr,
    hits: Arc<AtomicUsize>,
}

const MAX_REQUEST: usize = 1024;

async fn served() -> Served {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let router = Router::new().route(
        "/nodes/status",
        get(move || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            r#"{"ok":true}"#
        }),
    );
    let listen = format!("/ip4/127.0.0.1/tcp/{}", free_port());
    let node = dht::start_with_node_http(
        PeerTable::new(),
        config(&listen, None, RelaySettings::default()),
        NodeHttpSettings {
            max_request_bytes: MAX_REQUEST,
            ..NodeHttpSettings::default()
        },
    )
    .await;
    node.router_slot.set(router);
    let addr = format!("{listen}/p2p/{}", node.peer_id).parse().unwrap();
    Served { node, addr, hits }
}

/// An honest node that can reach `server` over the stream protocol.
async fn honest_client(server: &Served) -> (DhtHandle, NodeClient) {
    let peers = PeerTable::new();
    let client_node = dht::start(
        peers.clone(),
        config("/ip4/127.0.0.1/tcp/0", None, RelaySettings::default()),
    )
    .await;
    introduce(&peers, &server.node);
    let client = NodeClient::new().with_stream(client_node.node_http.clone());
    (client_node, client)
}

async fn honest_request_succeeds(client: &NodeClient, server: &Served) {
    let res = client
        .get(format!(
            "{}/nodes/status",
            p2p_base_url(&server.node.peer_id)
        ))
        .send()
        .await
        .expect("an honest peer is still served");
    assert_eq!(res.status(), StatusCode::OK);
}

fn frame(head: &str, declared_body: u32, body: &[u8]) -> Vec<u8> {
    let mut out = (head.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(&declared_body.to_be_bytes());
    out.extend_from_slice(body);
    out
}

const GOOD_HEAD: &str = r#"{"method":"GET","path_and_query":"/nodes/status","headers":[]}"#;

/// Whether a hostile exchange ended without the node answering anything.
fn refused(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Answered(bytes) => bytes.is_empty(),
        Outcome::Failed(_) => true,
    }
}

/// Malformed and oversized frames are refused by the stream codec before any handler runs. The
/// flooding cases keep writing after the bad length and stop only when the node resets the
/// stream, so a node that tried to read the claimed size would take the whole flood.
#[tokio::test]
async fn malformed_and_oversized_frames_are_refused_and_the_node_keeps_serving() {
    let server = served().await;
    let (_client_node, client) = honest_client(&server).await;
    let (mut hostile, written) = hostile_swarm(protocol_name(NETWORK));
    connect(&mut hostile, server.addr.clone(), server.node.peer_id).await;

    let flood = Duration::from_secs(2);
    let many_headers = format!(
        r#"{{"method":"GET","path_and_query":"/nodes/status","headers":[{}]}}"#,
        (0..64)
            .map(|i| format!(r#"["x-h{i}","v"]"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let long_path = format!(
        r#"{{"method":"GET","path_and_query":"/{}","headers":[]}}"#,
        "a".repeat(5000)
    );
    let bad_status_like = r#"{"method":42,"path_and_query":null}"#;
    let cases: Vec<(&str, Hostile, usize)> = vec![
        ("garbage bytes", Hostile::bytes(&[0xff; 64]), usize::MAX),
        ("empty stream", Hostile::bytes(&[]), usize::MAX),
        (
            "truncated length prefix",
            Hostile::bytes(&[0, 0]),
            usize::MAX,
        ),
        (
            "truncated header frame",
            Hostile::bytes(&[&100u32.to_be_bytes()[..], b"{\"method\""].concat()),
            usize::MAX,
        ),
        (
            "header frame length beyond the limit",
            Hostile::flooding(&u32::MAX.to_be_bytes(), flood),
            1 << 20,
        ),
        (
            "body length beyond the limit",
            Hostile::flooding(&frame(GOOD_HEAD, u32::MAX, &[]), flood),
            1 << 20,
        ),
        (
            "body one byte over the limit",
            Hostile::bytes(&frame(
                GOOD_HEAD,
                MAX_REQUEST as u32 + 1,
                &vec![0u8; MAX_REQUEST + 1],
            )),
            usize::MAX,
        ),
        (
            "body shorter than declared",
            Hostile::bytes(&frame(GOOD_HEAD, 100, &[1; 10])),
            usize::MAX,
        ),
        (
            "header frame that is not json",
            Hostile::bytes(&frame("not json at all", 0, &[])),
            usize::MAX,
        ),
        (
            "header fields of the wrong type",
            Hostile::bytes(&frame(bad_status_like, 0, &[])),
            usize::MAX,
        ),
        (
            "too many headers",
            Hostile::bytes(&frame(&many_headers, 0, &[])),
            usize::MAX,
        ),
        (
            "path beyond the limit",
            Hostile::bytes(&frame(&long_path, 0, &[])),
            usize::MAX,
        ),
    ];

    for (name, request, max_written) in cases {
        let before = written.load(Ordering::SeqCst);
        let started = Instant::now();
        let outcomes = send_all(&mut hostile, server.node.peer_id, vec![request]).await;
        assert!(refused(&outcomes[0]), "{name}: {:?}", outcomes[0]);
        assert!(
            written.load(Ordering::SeqCst) - before < max_written,
            "{name}: the node kept reading a refused frame"
        );
        assert!(started.elapsed() < Duration::from_secs(3), "{name}: slow");
        honest_request_succeeds(&client, &server).await;
    }
    // Only the honest requests reached the handler.
    assert_eq!(server.hits.load(Ordering::SeqCst), 12);
}

/// Hundreds of rapid hostile requests from one peer, mixed with honest ones from another, are
/// all settled; the honest peer is served and no hostile request reaches a handler.
#[tokio::test]
async fn a_flood_of_hostile_requests_does_not_starve_an_honest_peer() {
    let server = served().await;
    let (_client_node, client) = honest_client(&server).await;
    let (mut hostile, _) = hostile_swarm(protocol_name(NETWORK));
    connect(&mut hostile, server.addr.clone(), server.node.peer_id).await;

    let requests: Vec<Hostile> = (0..300)
        .map(|i| match i % 3 {
            0 => Hostile::bytes(&[0xff; 32]),
            1 => Hostile::bytes(&frame(GOOD_HEAD, u32::MAX, &[])),
            _ => Hostile::bytes(&frame(GOOD_HEAD, 64, &[7; 3])),
        })
        .collect();
    let flood = send_all(&mut hostile, server.node.peer_id, requests);
    let honest = async {
        for _ in 0..5 {
            honest_request_succeeds(&client, &server).await;
        }
    };
    let (outcomes, ()) = tokio::join!(flood, honest);
    assert_eq!(outcomes.len(), 300);
    assert!(
        outcomes.iter().all(refused),
        "a hostile request was answered"
    );
    assert_eq!(server.hits.load(Ordering::SeqCst), 5);
    honest_request_succeeds(&client, &server).await;
}

/// A well-formed request from a stranger still goes through the normal route checks: it is
/// answered, but only for allowed paths.
#[tokio::test]
async fn a_well_formed_hostile_request_gets_a_normal_answer_only_for_allowed_routes() {
    let server = served().await;
    let (mut hostile, _) = hostile_swarm(protocol_name(NETWORK));
    connect(&mut hostile, server.addr.clone(), server.node.peer_id).await;
    let denied = r#"{"method":"GET","path_and_query":"/admin/anything","headers":[]}"#;
    let outcomes = send_all(
        &mut hostile,
        server.node.peer_id,
        vec![Hostile::bytes(&frame(denied, 0, &[]))],
    )
    .await;
    let Outcome::Answered(bytes) = &outcomes[0] else {
        panic!("a well-formed request is answered: {:?}", outcomes[0]);
    };
    assert!(String::from_utf8_lossy(bytes).contains("path_not_allowed"));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}
