pub mod achievements;
pub mod attestations;
pub mod auth;
pub mod authz;
pub mod blocks;
pub mod channels;
pub mod connections;
pub mod conversations;
pub mod device_pairing;
pub mod devices;
pub mod discovery;
pub mod error;
pub mod friends;
pub mod guild_events;
pub mod guild_messages;
pub mod guilds;
pub mod handlers;
pub mod idempotency;
pub mod integrator_data;
pub mod integrator_schemas;
pub mod integrators;
pub mod migrate;
pub mod mirror_watcher;
pub mod outbox;
pub mod passkeys;
pub mod presence;
pub mod proto_schema;
pub mod recovery;
pub mod registry;
pub mod retention;
pub mod settlement;
pub mod state;
pub mod visibility;

use axum::http::{HeaderValue, Method};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use state::AppState;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

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
        // Issue #307: cross-device pairing, bootstrapping a session for a
        // client with no WebAuthn surface at all — see
        // `crate::device_pairing`'s module docs.
        .route("/auth/device/start", post(device_pairing::start_pairing))
        .route("/auth/device/poll", post(device_pairing::poll_pairing))
        .route(
            "/auth/device/approve",
            post(device_pairing::approve_pairing),
        )
        .route("/auth/device/deny", post(device_pairing::deny_pairing))
        .route("/me", get(handlers::me).patch(handlers::update_profile))
        .route("/me/history", get(handlers::my_history))
        .route("/me/achievements", get(attestations::list_my_achievements))
        .route(
            "/me/guild-announcements",
            get(guild_messages::list_my_guild_announcements),
        )
        .route("/identities/profiles", get(handlers::list_profiles))
        .route(
            "/identities/{id}/profile",
            get(handlers::get_identity_profile),
        )
        .route("/me/presence", put(presence::update_my_presence))
        .route(
            "/presence/{identity_id}",
            put(presence::update_integrator_presence),
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
            "/conversations",
            get(conversations::list_my_conversations).post(conversations::create_conversation),
        )
        .route(
            "/conversations/{id}/messages",
            get(conversations::list_messages).post(conversations::send_message),
        )
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
        // #290: `/integrations` is the single canonical path for every
        // integrator endpoint. #293 introduced it alongside a `/games`
        // back-compat alias; #290 generalized the whole vocabulary, so the
        // alias (and its read-redirect handlers) is gone rather than left
        // as the one remaining games-only spelling in the public API.
        .route(
            "/integrations",
            post(integrators::register_integrator).get(integrators::list_integrators),
        )
        .route("/integrations/{slug}", get(integrators::get_integrator))
        .route(
            "/integrations/{slug}/challenge",
            post(integrators::create_integrator_challenge),
        )
        .route("/integrations/whoami", get(integrators::integrator_whoami))
        // #84 (implementing #80's decided two-tier key model): key-set
        // management, both root-key-authenticated.
        .route(
            "/integrations/{slug}/keys",
            post(integrators::add_issuer_key).get(integrators::list_issuer_keys),
        )
        .route(
            "/integrations/{slug}/keys/{key_id}/revoke",
            post(integrators::revoke_issuer_key),
        )
        .route(
            "/integrations/{slug}/registry",
            get(registry::get_integrator_registry),
        )
        // Issue #95: the same read, under a dedicated top-level namespace —
        // the documented, stable external contract for anything that isn't
        // the Hub (a game's own tooling, a researcher, a future client).
        // `/integrations/{slug}/registry` above keeps working unchanged;
        // this is additive, not a replacement — see
        // `docs/architecture/registry.md`'s "External read surface"
        // section for the stability policy.
        .route("/registry/{slug}", get(registry::get_integrator_registry))
        .route(
            "/integrations/{slug}/achievements",
            get(achievements::list_achievement_definitions)
                .post(achievements::create_achievement_definition),
        )
        .route(
            "/integrations/{slug}/achievements/{key}",
            patch(achievements::update_achievement_definition),
        )
        // #32: issuance — a signed AchievementAttestation, gated on the
        // subject user's own achievements.issue grant (#28), not just
        // the integrator's own credential.
        .route(
            "/integrations/{slug}/achievements/{key}/issue",
            post(achievements::issue_achievement),
        )
        // #324/#325: the same claim-definition mechanism, App/Service's
        // own vocabulary ("milestone", not "achievement"). The claim
        // vocabulary is what stays category-specific here, not the path
        // prefix — both hang off the same `/integrations/{slug}` resource.
        .route(
            "/integrations/{slug}/milestones",
            get(achievements::list_milestone_definitions)
                .post(achievements::create_milestone_definition),
        )
        .route(
            "/integrations/{slug}/milestones/{key}",
            patch(achievements::update_milestone_definition),
        )
        .route(
            "/integrations/{slug}/milestones/{key}/issue",
            post(achievements::issue_milestone),
        )
        // #33: public read — authenticity + validity, deliberately no
        // recognition verdict (a consumer's own policy, never a server
        // boolean — see attestations.rs's module doc comment).
        .route("/attestations/{id}", get(attestations::get_attestation))
        // #85: revocation is a signed, appended entry, never a mutation of
        // the original attestation — only the original issuer may revoke.
        .route(
            "/attestations/{id}/revoke",
            post(attestations::revoke_attestation),
        )
        .route(
            "/integrations/{slug}/schemas",
            get(integrator_schemas::list_schema_versions)
                .post(integrator_schemas::publish_schema_version),
        )
        .route(
            "/integrations/{slug}/schemas/{version}",
            get(integrator_schemas::get_schema_version),
        )
        // #384 (implementing #381's decided policy): real instance data
        // against a published schema, and the read endpoint that enforces
        // the schema's (and any per-field override's) visibility.
        .route(
            "/integrations/{slug}/schemas/{version}/data",
            post(integrator_data::publish_instance),
        )
        .route(
            "/identities/{id}/integrator-data",
            get(integrator_data::get_identity_integrator_data),
        )
        .route(
            "/integrations/{slug}/connect",
            post(connections::connect).delete(connections::disconnect),
        )
        .route(
            "/integrations/{slug}/grants/{capability}",
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
        .route(
            "/guilds/{id}/integrations/{integrator_id}",
            post(guilds::associate_integrator),
        )
        .route(
            "/guilds/{id}/integrator-breakdown",
            get(guilds::game_breakdown),
        )
        .route(
            "/guilds/{id}/favorite-integrators",
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
            "/guilds/{id}/join-requests/mine",
            get(guilds::my_join_request),
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
            "/guilds/{id}/channels/{cid}/messages/archive",
            get(guild_messages::list_archive),
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
        // Issue #299: bulk entry content, the read path a mirror needs to
        // hold real ledger content, not just verify STHs — same public,
        // unauthenticated posture as the rest of this block.
        .route("/ledger/entries", get(settlement::list_entries))
        // Issue #313: node-to-node, bearer-authenticated — the one write
        // route in this block, unlike everything else above it.
        .route("/ledger/submit", post(settlement::submit_ledger_batch))
        .with_state(state)
        // Issue #265: every HTTP request gets a tracing span
        // (method/path/status/latency), and any `tracing::info!`/`error!`
        // call made while handling it is automatically correlated to that
        // span — this is what makes request-scoped log correlation work
        // without hand-threading a request id through every handler.
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer_from_env())
}
