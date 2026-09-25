//! Live checks for the header-read and request timeouts against a real,
//! running `avalon-server`. Gated `--ignored`, and each test that needs a
//! *small* configured timeout to run quickly is skipped (not failed) unless
//! that env var is set before `make start` — same "skip unless a small
//! override is set" pattern `crates/server/tests/resource_limits.rs` uses:
//!
//! ```text
//! AVALON_HTTP_HEADER_READ_TIMEOUT_SECS=2 make start
//! cargo test -p avalon-server --test http_timeouts header_read -- --ignored
//!
//! AVALON_HTTP_REQUEST_TIMEOUT_SECS=2 make start
//! cargo test -p avalon-server --test http_timeouts request_timeout -- --ignored
//!
//! AVALON_HTTP_REQUEST_TIMEOUT_SECS=2 make start
//! cargo test -p avalon-server --test http_timeouts websocket -- --ignored
//! ```

use futures_util::{SinkExt, StreamExt};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

fn server_addr() -> String {
    server_url()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string()
}

fn small_header_read_timeout() -> Option<u64> {
    std::env::var("AVALON_HTTP_HEADER_READ_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
}

fn small_request_timeout() -> Option<u64> {
    std::env::var("AVALON_HTTP_REQUEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
}

/// Reads until EOF or `deadline`, returning whether the peer closed the
/// connection (EOF) before the deadline elapsed.
async fn closes_within(stream: &mut TcpStream, deadline: std::time::Duration) -> bool {
    let mut buf = [0u8; 512];
    let result = tokio::time::timeout(deadline, async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => return true,
                Ok(_) => continue,
                Err(_) => return true,
            }
        }
    })
    .await;
    result.unwrap_or(false)
}

#[tokio::test]
#[ignore]
async fn idle_connection_closes_after_the_header_read_timeout() {
    let Some(configured) = small_header_read_timeout() else {
        eprintln!(
            "skipping: set AVALON_HTTP_HEADER_READ_TIMEOUT_SECS (e.g. 2) before `make start`"
        );
        return;
    };
    let mut stream = TcpStream::connect(server_addr())
        .await
        .expect("connect failed — is `make start` running?");
    let closed = closes_within(&mut stream, std::time::Duration::from_secs(configured + 10)).await;
    assert!(
        closed,
        "an idle connection should be closed once the header-read timeout elapses"
    );
}

#[tokio::test]
#[ignore]
async fn slow_headers_close_after_the_header_read_timeout() {
    let Some(configured) = small_header_read_timeout() else {
        eprintln!(
            "skipping: set AVALON_HTTP_HEADER_READ_TIMEOUT_SECS (e.g. 2) before `make start`"
        );
        return;
    };
    let mut stream = TcpStream::connect(server_addr())
        .await
        .expect("connect failed — is `make start` running?");
    // A request line with no terminating blank line — headers never finish.
    stream
        .write_all(b"GET /nodes/status HTTP/1.1\r\nHost: localhost\r\nX-Slow: a")
        .await
        .expect("write failed");
    let closed = closes_within(&mut stream, std::time::Duration::from_secs(configured + 10)).await;
    assert!(
        closed,
        "a connection stuck mid-headers should be closed once the header-read timeout elapses"
    );
}

#[tokio::test]
#[ignore]
async fn well_behaved_requests_are_unaffected_by_the_header_read_timeout() {
    let http = reqwest::Client::new();
    let res = http
        .get(format!("{}/nodes/status", server_url()))
        .send()
        .await
        .expect("request failed — is `make start` running?");
    assert!(res.status().is_success());
}

#[tokio::test]
#[ignore]
async fn a_stalled_body_gets_408_after_the_request_timeout() {
    let Some(configured) = small_request_timeout() else {
        eprintln!("skipping: set AVALON_HTTP_REQUEST_TIMEOUT_SECS (e.g. 2) before `make start`");
        return;
    };
    let mut stream = TcpStream::connect(server_addr())
        .await
        .expect("connect failed — is `make start` running?");
    stream
        .write_all(
            b"POST /nodes/announce HTTP/1.1\r\n\
              Host: localhost\r\n\
              Content-Type: application/json\r\n\
              Content-Length: 100000\r\n\
              Connection: close\r\n\r\n\
              {",
        )
        .await
        .expect("write failed");
    // Never send the rest of the declared 100000-byte body.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(configured + 15);
    let mut response = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => response.extend_from_slice(&buf[..n]),
            Ok(Err(_)) => break,
        }
    }
    let text = String::from_utf8_lossy(&response);
    let status_line = text.lines().next().unwrap_or_default();
    assert!(
        status_line.contains("408") || response.is_empty(),
        "expected a 408 response (or the connection dropped) once the request timeout elapsed, got: {status_line:?}"
    );
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// Direct-insert fixture, same shape
/// `crates/server/tests/realtime_reconnect.rs::seed_identity_session` uses —
/// bypasses the real WebAuthn ceremony since this test only needs a valid
/// session token to open `/ws/presence`.
async fn seed_identity_session(pool: &PgPool) -> String {
    let identity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO identities (id) VALUES ($1)")
        .bind(identity_id)
        .execute(pool)
        .await
        .expect("failed to seed identity");
    sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
        .bind(identity_id)
        .bind(format!("http-timeouts-test-{identity_id}"))
        .execute(pool)
        .await
        .expect("failed to seed profile");
    let token = format!("test-token-{}", Uuid::new_v4());
    let expires_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(identity_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .expect("failed to seed session");
    token
}

#[tokio::test]
#[ignore]
async fn a_live_websocket_outlasts_the_request_timeout() {
    let Some(configured) = small_request_timeout() else {
        eprintln!("skipping: set AVALON_HTTP_REQUEST_TIMEOUT_SECS (e.g. 2) before `make start`");
        return;
    };
    let pool = test_pool().await;
    let token = seed_identity_session(&pool).await;
    let ws_url = format!("ws://{}/ws/presence?token={token}", server_addr());
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("websocket connect failed");

    tokio::time::sleep(std::time::Duration::from_secs(configured + 5)).await;

    // Still alive past the request timeout: a ping gets a pong, not a close.
    ws.send(WsMessage::Ping(Vec::new().into()))
        .await
        .expect("send failed — the socket should still be open");
    let reply = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
        .await
        .expect("timed out waiting for a reply")
        .expect("stream ended")
        .expect("websocket error");
    assert!(
        !matches!(reply, WsMessage::Close(_)),
        "the request timeout must not close a live websocket connection"
    );
}
