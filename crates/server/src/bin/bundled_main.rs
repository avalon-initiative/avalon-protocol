//! `avalon-server-bundled`: the same server as `avalon-server`, plus a
//! managed, embedded Postgres started automatically when no `DATABASE_URL`
//! is supplied. Only built with the `bundled-postgres` feature — see
//! `Cargo.toml`'s `[[bin]]` entry and `avalon_server::bundled_postgres`'s
//! module doc comment.

#[tokio::main]
async fn main() {
    avalon_devenv::load();
    // Tracing initialized before starting the managed Postgres (rather than
    // inside `run_with_tracing`, plain `avalon-server`'s path via `run()`)
    // so that startup shows up in the logs too.
    let log_reload_handle = avalon_server::run::init_tracing();
    let data_dir = avalon_server::known_list::data_dir_from_env();
    let bundled = avalon_server::bundled_postgres::BundledPostgres::start_if_needed(&data_dir)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("refusing to start: {e}");
            std::process::exit(1);
        });

    match bundled {
        None => avalon_server::run::run_with_tracing(log_reload_handle).await,
        Some(postgres) => {
            tokio::select! {
                _ = avalon_server::run::run_with_tracing(log_reload_handle) => {}
                _ = shutdown_signal() => {
                    postgres.stop().await;
                }
            }
        }
    }
}

/// Resolves once SIGTERM (or, portably, Ctrl-C) is received, so the managed
/// Postgres started above gets a clean `stop()` instead of being killed
/// alongside the rest of the process.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
