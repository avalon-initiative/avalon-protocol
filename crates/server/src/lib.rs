pub mod auth;
pub mod error;
pub mod handlers;
pub mod migrate;
pub mod state;

use axum::routing::{get, post};
use axum::Router;
use state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/identities", post(handlers::register))
        .route("/sessions", post(handlers::login))
        .route("/me", get(handlers::me).patch(handlers::update_profile))
        .with_state(state)
}
