//! `avalon-server-bundled`: the same server as `avalon-server`, plus a
//! managed, embedded Postgres started automatically when no `DATABASE_URL`
//! is supplied. Only built with the `bundled-postgres` feature — see
//! `Cargo.toml`'s `[[bin]]` entry and `avalon_server::bundled_postgres`'s
//! module doc comment.

fn main() {
    avalon_server::cli_args::handle_or_exit(
        "avalon-server-bundled",
        env!("CARGO_PKG_VERSION"),
        "the Avalon Protocol node with a managed, embedded PostgreSQL",
    );
    run();
}

#[tokio::main]
async fn run() {
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

    // Returns after a graceful shutdown; the managed Postgres is stopped only once the
    // server has drained and released its connections.
    avalon_server::run::run_with_tracing(log_reload_handle).await;
    if let Some(postgres) = bundled {
        postgres.stop().await;
    }
}
