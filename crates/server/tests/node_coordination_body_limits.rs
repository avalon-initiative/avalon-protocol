//! Live checks for the explicit per-route-class request-body limits against
//! a real, running `avalon-server`. Gated `--ignored`; each test needs a
//! *small* configured limit to run quickly rather than transferring the
//! (deliberately generous) production defaults — same "skip unless a small
//! override is set" pattern `crates/server/tests/resource_limits.rs` uses:
//!
//! ```text
//! AVALON_NODE_COORDINATION_MAX_BODY_BYTES=4096 make start
//! cargo test -p avalon-server --test node_coordination_body_limits node_coordination -- --ignored
//!
//! AVALON_HTTP_MAX_BODY_BYTES=4096 make start
//! cargo test -p avalon-server --test node_coordination_body_limits general -- --ignored
//! ```

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn server_addr() -> String {
    std::env::var("AVALON_SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string()
}

/// Sends `POST <path>` with a `{"pad":"<filler>"}` JSON body of exactly
/// `body_len` bytes and returns the response status code (0 if the
/// connection closed before a status line arrived) — same technique
/// `crates/loadtest/src/scenarios.rs::oversized` uses: the body's byte
/// count is what's under test, not whether it deserializes into anything.
async fn post_body_of_len(path: &str, body_len: usize) -> u16 {
    let overhead = "{\"pad\":\"\"}".len();
    let filler = body_len.saturating_sub(overhead);
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n{{\"pad\":\""
    );
    let mut stream = TcpStream::connect(server_addr())
        .await
        .expect("connect failed — is `make start` running?");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("write failed");
    let chunk = vec![b'a'; 64 * 1024];
    let mut left = filler;
    while left > 0 {
        let n = left.min(chunk.len());
        if stream.write_all(&chunk[..n]).await.is_err() {
            break;
        }
        left -= n;
    }
    let _ = stream.write_all(b"\"}").await;
    let mut buf = [0u8; 512];
    match tokio::time::timeout(std::time::Duration::from_secs(30), stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => std::str::from_utf8(&buf[..n])
            .ok()
            .and_then(|t| t.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0),
        _ => 0,
    }
}

#[tokio::test]
#[ignore]
async fn node_coordination_route_accepts_a_body_at_the_configured_limit() {
    let Some(limit) = std::env::var("AVALON_NODE_COORDINATION_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        eprintln!(
            "skipping: set AVALON_NODE_COORDINATION_MAX_BODY_BYTES (e.g. 4096) before `make start`"
        );
        return;
    };
    let status = post_body_of_len("/nodes/announce", limit).await;
    assert_ne!(
        status, 413,
        "a body at exactly the node-coordination limit should reach the handler"
    );
}

#[tokio::test]
#[ignore]
async fn node_coordination_route_rejects_a_body_one_byte_over_the_limit() {
    let Some(limit) = std::env::var("AVALON_NODE_COORDINATION_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        eprintln!(
            "skipping: set AVALON_NODE_COORDINATION_MAX_BODY_BYTES (e.g. 4096) before `make start`"
        );
        return;
    };
    let status = post_body_of_len("/nodes/announce", limit + 1).await;
    assert_eq!(
        status, 413,
        "a body one byte over the node-coordination limit should be rejected"
    );
}

#[tokio::test]
#[ignore]
async fn general_route_accepts_a_body_at_the_configured_limit() {
    let Some(limit) = std::env::var("AVALON_HTTP_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        eprintln!("skipping: set AVALON_HTTP_MAX_BODY_BYTES (e.g. 4096) before `make start`");
        return;
    };
    let status = post_body_of_len("/identities/register/start", limit).await;
    assert_ne!(
        status, 413,
        "a body at exactly the general limit should reach the handler"
    );
}

#[tokio::test]
#[ignore]
async fn general_route_rejects_a_body_one_byte_over_the_limit() {
    let Some(limit) = std::env::var("AVALON_HTTP_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        eprintln!("skipping: set AVALON_HTTP_MAX_BODY_BYTES (e.g. 4096) before `make start`");
        return;
    };
    let status = post_body_of_len("/identities/register/start", limit + 1).await;
    assert_eq!(
        status, 413,
        "a body one byte over the general limit should be rejected"
    );
}
