pub mod auth;
pub mod error;
pub mod friends;
pub mod handlers;
pub mod migrate;
pub mod outbox;
pub mod state;

use axum::routing::{delete, get, post};
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
        .route(
            "/friends/requests",
            get(friends::list_friend_requests).post(friends::create_friend_request),
        )
        .route(
            "/friends/requests/:id/accept",
            post(friends::accept_friend_request),
        )
        .route(
            "/friends/requests/:id",
            delete(friends::decline_or_withdraw_friend_request),
        )
        .route("/friends", get(friends::list_friends))
        .route("/friends/:identity_id", delete(friends::remove_friend))
        .with_state(state)
}
