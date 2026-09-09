pub mod achievements;
pub mod auth;
pub mod authz;
pub mod blocks;
pub mod channels;
pub mod connections;
pub mod devices;
pub mod error;
pub mod friends;
pub mod games;
pub mod guild_messages;
pub mod guilds;
pub mod handlers;
pub mod migrate;
pub mod outbox;
pub mod presence;
pub mod state;

use axum::http::{HeaderValue, Method};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use state::AppState;
use tower_http::cors::CorsLayer;

/// The Hub (and any other browser client) is a different origin than
/// `avalon-server` by construction (issue #77 — Hub is a client, never
/// bundled with the backend), so every browser request here is
/// cross-origin and needs an explicit CORS grant or the browser blocks it
/// before the request is even sent. Origin is configurable via
/// `AVALON_HUB_ORIGIN` (comma-separated for more than one) since it varies
/// per deployment; defaults to the Hub's Vite dev server for local dev.
fn cors_layer_from_env() -> CorsLayer {
    let origins =
        std::env::var("AVALON_HUB_ORIGIN").unwrap_or_else(|_| "http://localhost:5173".to_string());
    let allowed: Vec<HeaderValue> = origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse()
                .unwrap_or_else(|_| panic!("AVALON_HUB_ORIGIN contains an invalid origin: {s}"))
        })
        .collect();

    CorsLayer::new()
        .allow_origin(allowed)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
}

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
        .route("/me/history", get(handlers::my_history))
        .route("/identities/profiles", get(handlers::list_profiles))
        .route("/me/presence", put(presence::update_my_presence))
        .route("/presence", get(presence::get_presence))
        .route("/ws/presence", get(presence::presence_ws))
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
        .route("/friends/handle/:handle", get(friends::resolve_handle))
        .route(
            "/blocks",
            get(blocks::list_blocks).post(blocks::create_block),
        )
        .route("/blocks/:identity_id", delete(blocks::remove_block))
        .route(
            "/me/devices/grants",
            get(devices::list_device_grants).post(devices::request_device_grant),
        )
        .route("/me/devices/grants/:id", get(devices::get_device_grant))
        .route(
            "/me/devices/grants/:id/approve",
            post(devices::approve_device_grant),
        )
        .route("/me/devices", get(devices::list_devices))
        .route("/me/devices/:id", patch(devices::rename_device))
        .route("/me/devices/:id/revoke", post(devices::revoke_device))
        .route("/games", post(games::register_game))
        .route("/games/:slug", get(games::get_game))
        .route("/games/:slug/challenge", post(games::create_game_challenge))
        .route("/games/whoami", get(games::game_whoami))
        .route(
            "/games/:slug/achievements",
            get(achievements::list_achievement_definitions)
                .post(achievements::create_achievement_definition),
        )
        .route(
            "/games/:slug/achievements/:key",
            patch(achievements::update_achievement_definition),
        )
        .route(
            "/games/:slug/connect",
            post(connections::connect).delete(connections::disconnect),
        )
        .route(
            "/games/:slug/grants/:capability",
            delete(connections::revoke_grant),
        )
        .route("/me/connections", get(connections::list_my_connections))
        .route("/me/grants", get(connections::my_grants))
        .route("/guilds", post(guilds::create_guild))
        .route(
            "/guilds/:id",
            get(guilds::get_guild).patch(guilds::update_guild),
        )
        .route(
            "/guilds/:id/roles",
            get(guilds::list_roles).post(guilds::create_role),
        )
        .route("/guilds/:id/roles/:idx", patch(guilds::update_role))
        .route(
            "/guilds/:id/transfer-ownership",
            post(guilds::transfer_ownership),
        )
        .route("/guilds/:id/games/:game_id", post(guilds::associate_game))
        .route("/guilds/:id/invites", post(guilds::create_invite))
        .route(
            "/guilds/:id/invites/:invite_id/accept",
            post(guilds::accept_invite),
        )
        .route(
            "/guilds/:id/invites/:invite_id/decline",
            post(guilds::decline_invite),
        )
        .route("/guilds/:id/join", post(guilds::join_guild))
        .route("/guilds/:id/leave", post(guilds::leave_guild))
        .route("/guilds/:id/members", get(guilds::list_members))
        .route(
            "/guilds/:id/members/:identity_id",
            patch(guilds::update_member_role).delete(guilds::remove_member),
        )
        .route("/me/guilds", get(guilds::list_my_guilds))
        .route(
            "/guilds/:id/channels",
            get(channels::list_channels).post(channels::create_channel),
        )
        .route("/guilds/:id/channels/:cid", patch(channels::update_channel))
        .route(
            "/guilds/:id/channels/:cid/archive",
            post(channels::archive_channel),
        )
        .route(
            "/guilds/:id/channels/:cid/messages",
            get(guild_messages::list_messages).post(guild_messages::send_message),
        )
        .route(
            "/guilds/:id/channels/:cid/messages/:mid",
            delete(guild_messages::delete_message),
        )
        .with_state(state)
        .layer(cors_layer_from_env())
}
