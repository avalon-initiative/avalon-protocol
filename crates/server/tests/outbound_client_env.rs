//! Failover clients ignore proxy environment variables and redirects. Alone in its own process
//! because it sets an environment variable that other clients would read.

use std::net::SocketAddr;
use std::time::Duration;

use avalon_server::node_http::p2p_base_url;
use avalon_server::outbound_policy::{CheckedTarget, OutboundPolicy};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn server(template: ResponseTemplate) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(template)
        .mount(&s)
        .await;
    s
}

fn pinned_to(addr: SocketAddr) -> CheckedTarget {
    CheckedTarget {
        base_url: format!("http://pinned.invalid:{}", addr.port()),
        pinned_host: Some("pinned.invalid".to_string()),
        addr,
        policy: OutboundPolicy::new(true),
    }
}

#[tokio::test]
async fn failover_clients_use_no_proxy_and_follow_no_redirects() {
    let proxy = server(ResponseTemplate::new(200)).await;
    let target_server =
        server(ResponseTemplate::new(302).insert_header("location", proxy.uri())).await;
    let id = libp2p::PeerId::random();
    let p2p = OutboundPolicy::new(true)
        .check_node_url(&p2p_base_url(&id))
        .await
        .unwrap();

    unsafe { std::env::set_var("HTTP_PROXY", proxy.uri()) };
    let checked = pinned_to(*target_server.address()).client(Duration::from_secs(5));
    let node = p2p.node_client(Duration::from_secs(5));
    unsafe { std::env::remove_var("HTTP_PROXY") };

    // Straight to the pinned address, and the redirect is returned rather than followed.
    let res = checked
        .get(format!(
            "http://pinned.invalid:{}",
            target_server.address().port()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 302);
    assert_eq!(target_server.received_requests().await.unwrap().len(), 1);

    let res = node.get(target_server.uri()).send().await.unwrap();
    assert_eq!(res.status(), 302);
    assert_eq!(target_server.received_requests().await.unwrap().len(), 2);
    assert!(proxy.received_requests().await.unwrap().is_empty());
}
