pub mod auth;
pub mod error;
pub mod handlers;
pub mod migrate;
pub mod outbox;
pub mod state;

use axum::routing::{get, post};
use axum::Router;
use state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/identities/register/start", post(handlers::register_start))
        .route(
            "/identities/register/finish",
            post(handlers::register_finish),
        )
        .route("/sessions/start", post(handlers::session_start))
        .route("/sessions/finish", post(handlers::session_finish))
        .route("/me", get(handlers::me).patch(handlers::update_profile))
        .with_state(state)
}
