//! `cargo run -p avalon-server --bin migrate -- up|down|reset` — see the
//! Makefile's `make migrate`/`make migrate-down`/`make db-reset` targets,
//! which are the intended entry points.

use avalon_server::migrate;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() {
    avalon_devenv::load();
    let direction = std::env::args().nth(1).unwrap_or_else(|| "up".to_string());

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");
    let dir = migrate::dir_override_from_env();
    let source = match &dir {
        Some(d) => migrate::MigrationSource::Dir(d),
        None => migrate::MigrationSource::Embedded,
    };

    match direction.as_str() {
        "up" => {
            migrate::migrate_up(&pool, source)
                .await
                .expect("migration failed");
            println!("migrations applied");
        }
        "down" => {
            migrate::migrate_down_one(&pool, source)
                .await
                .expect("revert failed");
            println!("last migration reverted");
        }
        "reset" => {
            migrate::reset(&pool, source).await.expect("reset failed");
            println!("database reset and migrations reapplied");
        }
        other => {
            eprintln!("unknown direction: {other:?} — expected \"up\", \"down\", or \"reset\"");
            std::process::exit(1);
        }
    }
}
