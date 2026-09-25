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
use tower::Service;

/// Accepts connections from `listener` and serves `app` on each, applying
/// `header_read_timeout` to every connection's HTTP/1 header parsing.
pub async fn serve(listener: TcpListener, app: Router, header_read_timeout: Duration) -> ! {
    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!("accept error: {err}");
                continue;
            }
        };
        let tower_service = app.clone();

        tokio::spawn(async move {
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

            if let Err(err) = builder
                .serve_connection(io, hyper_service)
                .with_upgrades()
                .await
            {
                tracing::trace!(%remote_addr, "connection closed: {err:#}");
            }
        });
    }
}
