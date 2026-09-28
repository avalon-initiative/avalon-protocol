//! A header-read-timeout-aware replacement for `axum::serve`, which builds
//! its own connection builder internally with no way to configure one.
//!
//! Hyper re-arms the same header-read timer between keep-alive requests, so
//! this one setting also bounds a connection that never sends a byte at all.

use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::Router;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tower::Service;

use crate::shutdown::Shutdown;

/// Accepts connections from `listener` and serves `app` on each, applying
/// `header_read_timeout` to every connection's HTTP/1 header parsing. On
/// shutdown it stops accepting, asks every connection to finish its in-flight
/// request and close, and waits up to `drain_timeout` for them.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    header_read_timeout: Duration,
    mut shutdown: Shutdown,
    drain_timeout: Duration,
) {
    let mut connections: JoinSet<()> = JoinSet::new();
    loop {
        let (stream, remote_addr) = tokio::select! {
            _ = shutdown.requested() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::warn!("accept error: {err}");
                    continue;
                }
            },
        };
        let tower_service = app.clone();
        let mut conn_shutdown = shutdown.clone();

        connections.spawn(async move {
            let io = TokioIo::new(stream);
            let hyper_service = service_fn(move |mut request: hyper::Request<Incoming>| {
                request.extensions_mut().insert(ConnectInfo(remote_addr));
                // hyper's `service_fn` only gives `&self`, so each call gets
                // its own owned clone to call `Service::call` (`&mut self`) on.
                tower_service.clone().call(request.map(Body::new))
            });

            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(header_read_timeout);

            let conn = builder.serve_connection(io, hyper_service).with_upgrades();
            tokio::pin!(conn);
            let result = tokio::select! {
                r = conn.as_mut() => r,
                _ = conn_shutdown.requested() => {
                    conn.as_mut().graceful_shutdown();
                    conn.as_mut().await
                }
            };
            if let Err(err) = result {
                tracing::trace!(%remote_addr, "connection closed: {err:#}");
            }
        });
    }

    drop(listener);
    tracing::info!(open = connections.len(), "draining in-flight connections");
    let drained = tokio::time::timeout(drain_timeout, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            remaining = connections.len(),
            "drain timeout reached; closing remaining connections"
        );
        connections.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn start(
        handler_delay: Duration,
        drain: Duration,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    ) {
        let app = Router::new().route(
            "/slow",
            get(move || async move {
                tokio::time::sleep(handler_delay).await;
                "done"
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, shutdown) = Shutdown::manual();
        let task = tokio::spawn(serve(
            listener,
            app,
            Duration::from_secs(5),
            shutdown,
            drain,
        ));
        (addr, tx, task)
    }

    async fn send_slow_request(addr: std::net::SocketAddr) -> tokio::net::TcpStream {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /slow HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        stream
    }

    #[tokio::test]
    async fn in_flight_request_completes_before_serve_returns() {
        let (addr, tx, task) = start(Duration::from_millis(300), Duration::from_secs(5)).await;
        let mut stream = send_slow_request(addr).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(true).unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).await.unwrap();
        assert!(body.contains("200 OK") && body.ends_with("done"), "{body}");
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "listener must be closed after shutdown"
        );
    }

    #[tokio::test]
    async fn stuck_request_is_cut_off_at_the_drain_timeout() {
        let (addr, tx, task) = start(Duration::from_secs(3600), Duration::from_millis(200)).await;
        let _stream = send_slow_request(addr).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("serve must return once the drain timeout passes")
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
