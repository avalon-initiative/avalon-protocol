pub mod achievements;
pub mod auth;
pub mod authz;
pub mod blocks;
pub mod channels;
pub mod connections;
pub mod devices;
pub mod discovery;
pub mod error;
pub mod friends;
pub mod games;
pub mod guild_events;
pub mod guild_messages;
pub mod guilds;
pub mod handlers;
pub mod migrate;
pub mod outbox;
pub mod passkeys;
pub mod presence;
pub mod recovery;
pub mod retention;
pub mod settlement;
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
        .route(
            "/presence/{identity_id}",
            put(presence::update_game_presence),
        )
        .route("/presence", get(presence::get_presence))
        .route("/ws/presence", get(presence::presence_ws))
        .route(
            "/friends/requests",
            get(friends::list_friend_requests).post(friends::create_friend_request),
        )
        .route(
            "/friends/requests/{id}/accept",
            post(friends::accept_friend_request),
        )
        .route(
            "/friends/requests/{id}",
            delete(friends::decline_or_withdraw_friend_request),
        )
        .route("/friends", get(friends::list_friends))
        .route("/friends/{identity_id}", delete(friends::remove_friend))
        .route("/friends/handle/{handle}", get(friends::resolve_handle))
        .route("/people/discover", get(discovery::discover_people))
        .route("/identities/search", get(discovery::search_identities))
        .route(
            "/blocks",
            get(blocks::list_blocks).post(blocks::create_block),
        )
        .route("/blocks/{identity_id}", delete(blocks::remove_block))
        .route(
            "/me/devices/grants",
            get(devices::list_device_grants).post(devices::request_device_grant),
        )
        .route("/me/devices/grants/{id}", get(devices::get_device_grant))
        .route(
            "/me/devices/grants/{id}/approve",
            post(devices::approve_device_grant),
        )
        .route("/me/devices", get(devices::list_devices))
        .route("/me/devices/{id}", patch(devices::rename_device))
        .route("/me/devices/{id}/revoke", post(devices::revoke_device))
        .route(
            "/me/passkeys/register/start",
            post(passkeys::register_start),
        )
        .route(
            "/me/passkeys/register/finish",
            post(passkeys::register_finish),
        )
        .route("/me/passkeys", get(passkeys::list_passkeys))
        .route("/me/passkeys/{id}", patch(passkeys::rename_passkey))
        .route("/me/passkeys/{id}/revoke", post(passkeys::revoke_passkey))
        // Issue #201: social recovery. Guardian configuration and the
        // caller's own status are session-authenticated (`/me/...`);
        // `/recovery/requests/start` and `/finish` are the one deliberate
        // exception (see `crate::recovery` module docs — the caller by
        // definition has no valid session for the identity being
        // recovered). `/recovery/requests/:id` (GET) and
        // `/identities/:id/recovery/status` are public by design, per the
        // ticket's "mandatory *public* time-delay" invariant.
        .route(
            "/me/recovery/guardians",
            get(recovery::get_guardians).put(recovery::set_guardians),
        )
        .route("/me/recovery/status", get(recovery::my_recovery_status))
        .route(
            "/me/recovery/guardian-requests",
            get(recovery::guardian_requests),
        )
        .route("/recovery/requests/start", post(recovery::start_request))
        .route("/recovery/requests/finish", post(recovery::finish_request))
        .route("/recovery/requests/{id}", get(recovery::get_request))
        .route(
            "/recovery/requests/{id}/approve",
            post(recovery::approve_request),
        )
        .route(
            "/recovery/requests/{id}/cancel",
            post(recovery::cancel_request),
        )
        .route(
            "/recovery/requests/{id}/finalize",
            post(recovery::finalize_request),
        )
        .route(
            "/identities/{id}/recovery/status",
            get(recovery::identity_recovery_status),
        )
        .route("/games", post(games::register_game))
        .route("/games/{slug}", get(games::get_game))
        .route(
            "/games/{slug}/challenge",
            post(games::create_game_challenge),
        )
        .route("/games/whoami", get(games::game_whoami))
        .route(
            "/games/{slug}/achievements",
            get(achievements::list_achievement_definitions)
                .post(achievements::create_achievement_definition),
        )
        .route(
            "/games/{slug}/achievements/{key}",
            patch(achievements::update_achievement_definition),
        )
        .route(
            "/games/{slug}/connect",
            post(connections::connect).delete(connections::disconnect),
        )
        .route(
            "/games/{slug}/grants/{capability}",
            delete(connections::revoke_grant),
        )
        .route("/me/connections", get(connections::list_my_connections))
        .route("/me/grants", get(connections::my_grants))
        .route("/guilds", post(guilds::create_guild))
        .route("/guilds/discover", get(guilds::discover_guilds))
        .route(
            "/guilds/{id}",
            get(guilds::get_guild).patch(guilds::update_guild),
        )
        .route(
            "/guilds/{id}/roles",
            get(guilds::list_roles).post(guilds::create_role),
        )
        .route(
            "/guilds/{id}/roles/{idx}",
            patch(guilds::update_role).delete(guilds::delete_role),
        )
        .route(
            "/guilds/{id}/permission-overrides",
            get(guilds::list_permission_overrides).put(guilds::set_permission_override),
        )
        .route(
            "/guilds/{id}/permission-overrides/{override_id}",
            delete(guilds::delete_permission_override),
        )
        .route(
            "/guilds/{id}/transfer-ownership",
            post(guilds::transfer_ownership),
        )
        .route("/guilds/{id}/games/{game_id}", post(guilds::associate_game))
        .route("/guilds/{id}/game-breakdown", get(guilds::game_breakdown))
        .route(
            "/guilds/{id}/favorite-games",
            get(guilds::list_favorite_games).put(guilds::set_favorite_games),
        )
        .route("/guilds/{id}/invites", post(guilds::create_invite))
        .route(
            "/guilds/{id}/invites/{invite_id}/accept",
            post(guilds::accept_invite),
        )
        .route(
            "/guilds/{id}/invites/{invite_id}/decline",
            post(guilds::decline_invite),
        )
        .route("/guilds/{id}/join", post(guilds::join_guild))
        .route(
            "/guilds/{id}/join-requests",
            get(guilds::list_join_requests).post(guilds::create_join_request),
        )
        .route(
            "/guilds/{id}/join-requests/{request_id}/approve",
            post(guilds::approve_join_request),
        )
        .route(
            "/guilds/{id}/join-requests/{request_id}/reject",
            post(guilds::reject_join_request),
        )
        .route(
            "/guilds/{id}/join-requests/{request_id}",
            delete(guilds::withdraw_join_request),
        )
        .route("/guilds/{id}/leave", post(guilds::leave_guild))
        .route("/guilds/{id}/members", get(guilds::list_members))
        .route(
            "/guilds/{id}/members/{identity_id}",
            patch(guilds::update_member_role).delete(guilds::remove_member),
        )
        .route("/me/guilds", get(guilds::list_my_guilds))
        .route(
            "/guilds/{id}/channels",
            get(channels::list_channels).post(channels::create_channel),
        )
        .route(
            "/guilds/{id}/channels/{cid}",
            patch(channels::update_channel),
        )
        .route(
            "/guilds/{id}/channels/{cid}/archive",
            post(channels::archive_channel),
        )
        .route(
            "/guilds/{id}/channels/{cid}/messages",
            get(guild_messages::list_messages).post(guild_messages::send_message),
        )
        .route(
            "/guilds/{id}/channels/{cid}/messages/{mid}",
            delete(guild_messages::delete_message),
        )
        .route(
            "/guilds/{id}/events",
            get(guild_events::list_events).post(guild_events::create_event),
        )
        .route(
            "/guilds/{id}/events/{eid}",
            patch(guild_events::update_event).delete(guild_events::delete_event),
        )
        .route(
            "/guilds/{id}/events/{eid}/rsvp",
            put(guild_events::upsert_rsvp),
        )
        .route(
            "/guilds/{id}/events/{eid}/rsvps",
            get(guild_events::list_rsvps),
        )
        // Issue #211: public, unauthenticated mirror-facing transparency-log
        // reads — see `crate::settlement`'s module docs for why these carry
        // no auth requirement, unlike everything else in this router.
        .route("/ledger/sth/latest", get(settlement::latest_sth))
        .route("/ledger/sth/{tree_size}", get(settlement::sth_at_tree_size))
        .route(
            "/ledger/proof/consistency",
            get(settlement::consistency_proof),
        )
        .route("/ledger/proof/inclusion", get(settlement::inclusion_proof))
        .with_state(state)
        .layer(cors_layer_from_env())
}
