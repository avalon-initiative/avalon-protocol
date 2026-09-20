pub mod achievements;
pub mod admin;
pub mod attestations;
pub mod auth;
pub mod authz;
pub mod blocks;
pub mod channels;
pub mod chat;
pub mod chat_replication;
pub mod connections;
pub mod continuation;
pub mod conversations;
pub mod cross_node_login;
pub mod cross_shard;
pub mod cross_shard_fetch;
pub mod device_pairing;
pub mod devices;
pub mod dht;
pub mod discovery;
pub mod error;
pub mod friends;
pub mod guild_events;
pub mod guild_messages;
pub mod guilds;
pub mod handlers;
pub mod idempotency;
pub mod identity_locator;
pub mod integrator_data;
pub mod integrator_schema_mappings;
pub mod integrator_schemas;
pub mod integrators;
pub mod interest;
pub mod internal_role;
pub mod issuer_registration;
pub mod migrate;
pub mod mirror_push;
pub mod mirror_watcher;
pub mod nodes;
pub mod outbox;
pub mod passkeys;
pub mod presence;
pub mod proto_schema;
pub mod realtime_relay;
pub mod rebuild;
pub mod recognitions;
pub mod recovery;
pub mod redis_limits;
pub mod registry;
pub mod replication;
pub mod resources;
pub mod retention;
pub mod settlement;
pub mod state;
pub mod version;
pub mod visibility;

use axum::http::{HeaderValue, Method, Request};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use state::AppState;
use tower::limit::ConcurrencyLimitLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::{KeyExtractor, PeerIpKeyExtractor};
use tower_governor::{GovernorError, GovernorLayer};
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

/// Issue #363, implementing #287's decision: hoster-configurable resource
/// limits, every one defaulted so an unconfigured node behaves exactly as
/// it always has — never "unlimited", never "fails to start".
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 256;
const DEFAULT_RATE_LIMIT_PER_MINUTE: u64 = 600;

pub(crate) fn max_concurrent_requests_from_env() -> usize {
    std::env::var("AVALON_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_CONCURRENT_REQUESTS)
}

pub(crate) fn rate_limit_per_minute_from_env() -> u64 {
    std::env::var("AVALON_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE)
}

/// Keyed by the calling integrator's own key id
/// (`x-avalon-integrator-key-id`, the same header
/// [`authz::authenticate_integrator`] reads) when present, so one
/// integrator's traffic can't starve another's — falls back to peer IP for
/// pre-auth endpoints (registration, login) that don't carry an integrator
/// key yet. Requires the server to be served via
/// `into_make_service_with_connect_info::<SocketAddr>()` (see `main.rs`)
/// for the IP fallback to resolve to anything but an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IntegratorOrIpKeyExtractor;

impl KeyExtractor for IntegratorOrIpKeyExtractor {
    type Key = String;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        if let Some(key_id) = req
            .headers()
            .get("x-avalon-integrator-key-id")
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
        {
            return Ok(format!("integrator:{key_id}"));
        }
        PeerIpKeyExtractor.extract(req).map(|ip| format!("ip:{ip}"))
    }
}

/// `redis_limiter` is `Some` only when `AVALON_REDIS_URL` is configured
/// (issue #545) — built by the caller (`main.rs`), since connecting to
/// Redis is async and this function isn't. `None` (the default) keeps
/// every layer below exactly as it's always been; see
/// `crate::redis_limits`'s own module doc comment for what changes when
/// it's `Some`.
pub fn router(state: AppState, redis_limiter: Option<redis_limits::RedisLimiterState>) -> Router {
    let max_concurrent_requests = max_concurrent_requests_from_env();
    let rate_limit_per_minute = rate_limit_per_minute_from_env();
    let governor_config = GovernorConfigBuilder::default()
        .period(std::time::Duration::from_secs_f64(
            60.0 / rate_limit_per_minute as f64,
        ))
        .burst_size(rate_limit_per_minute as u32)
        .key_extractor(IntegratorOrIpKeyExtractor)
        .finish()
        .expect("per_minute is always > 0, so period/burst_size are always non-zero");

    let router = Router::new()
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
        // Epic #623, issue #634: cross-node login — see
        // `crate::cross_node_login`'s module docs.
        .route("/auth/cross-node/start", post(cross_node_login::start))
        .route("/auth/cross-node/poll", post(cross_node_login::poll))
        .route("/auth/cross-node/submit", post(cross_node_login::submit))
        .route("/auth/cross-node/deny", post(cross_node_login::deny))
        // Epic #623, issue #639's own gap: an approval screen needs to
        // read a pending request's context before deciding — see
        // `crate::cross_node_login::lookup`'s own doc comment.
        .route("/auth/cross-node/lookup", get(cross_node_login::lookup))
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
        // Epic #623, issue #635: identity locator — see
        // `crate::identity_locator`'s module docs.
        .route(
            "/identities/{id}/locations",
            get(identity_locator::get_locations),
        )
        .route("/me/presence", put(presence::update_my_presence))
        .route(
            "/presence/{identity_id}",
            put(presence::update_integrator_presence),
        )
        .route("/presence", get(presence::get_presence))
        .route("/ws/presence", get(presence::presence_ws))
        .route("/ws/messages", get(chat::chat_ws))
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
        .route("/me/recovery/guardian-of", get(recovery::guardian_of))
        .route(
            "/me/recovery/guardian-of/{identity_id}",
            delete(recovery::resign_guardian),
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
        // #481 (implementing #479's decided ADR): per-network issuer
        // registration gate — deliberately not nested under
        // /integrations/{slug}/... since the wire shape is pubkey-first,
        // not integrator-id-first (see issuer_registration.rs's module
        // doc comment for why this is a separate table/concept).
        .route(
            "/issuers/registration-challenge",
            post(issuer_registration::create_registration_challenge),
        )
        .route(
            "/issuers/register",
            post(issuer_registration::register_issuer),
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
        // Issue #89: public recognition relationships — an integrator
        // declaring "I recognize <other integrator>'s claims, for
        // <scope>" as a durable fact, queryable in both directions.
        .route(
            "/integrations/{slug}/recognitions",
            post(recognitions::publish_recognition).get(recognitions::list_recognitions),
        )
        .route(
            "/integrations/{slug}/recognitions/revoke",
            post(recognitions::revoke_recognition),
        )
        .route(
            "/integrations/{slug}/recognized-by",
            get(recognitions::list_recognized_by),
        )
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
        // #495 (implementing #492's decided shape): N ordinary attestations
        // sharing one request/signature envelope — a literal path segment
        // alongside `{key}/issue` above, same precedent `/ledger/sth/latest`
        // vs. `/ledger/sth/{tree_size}` already established.
        .route(
            "/integrations/{slug}/achievements/bulk-issue",
            post(achievements::bulk_issue_achievements),
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
        .route(
            "/integrations/{slug}/milestones/bulk-issue",
            post(achievements::bulk_issue_milestones),
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
        // #491: schema-to-schema mapping model — documents a correspondence
        // between two of an integrator's own published schema versions,
        // never an execution engine (see module doc comment).
        .route(
            "/integrations/{slug}/mappings",
            get(integrator_schema_mappings::list_mappings)
                .post(integrator_schema_mappings::publish_mapping),
        )
        .route(
            "/integrations/{slug}/mappings/{seq}",
            get(integrator_schema_mappings::get_mapping),
        )
        // #384 (implementing #381's decided policy): real instance data
        // against a published schema, and the read endpoint that enforces
        // the schema's (and any per-field override's) visibility.
        .route(
            "/integrations/{slug}/schemas/{version}/data",
            post(integrator_data::publish_instance),
        )
        // #533: append-only tombstone for a published instance (e.g. a
        // deleted character) — never a physical delete, see
        // `integrator_data::delete_instance`'s own doc comment.
        .route(
            "/integrations/{slug}/schemas/{version}/data/{subject}",
            delete(integrator_data::delete_instance),
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
        .route("/me/guild-invites", get(guilds::my_guild_invites))
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
        // Issue #531: managed-hosting two-phase remote signing — see
        // `crate::settlement::prepare_batch`/`finalize_batch`.
        .route("/ledger/prepare-batch", post(settlement::prepare_batch))
        .route("/ledger/finalize-batch", post(settlement::finalize_batch))
        // Issue #529: cross-shard root — see `crate::cross_shard`.
        .route(
            "/ledger/cross-shard-root",
            get(cross_shard::cross_shard_root),
        )
        // Issue #526: forwarding-node discovery hint — see
        // `crate::settlement::remote_submit_status`.
        .route(
            "/ledger/remote-submit-status",
            get(settlement::remote_submit_status),
        )
        // Issue #569: archive-confirmation gating's own read — see
        // `crate::settlement::mirror_progress`.
        .route("/ledger/mirror-progress", get(settlement::mirror_progress))
        // Issue #362: node-to-node peer discovery — no auth, same public
        // posture as the `/ledger/*` block above, since a peer table isn't
        // sensitive the way ledger-write endpoints are.
        .route("/nodes/announce", post(nodes::announce))
        .route("/nodes/peers", get(nodes::list_peers))
        .route("/nodes/status", get(nodes::status))
        // Issue #658: hoster-only runtime log-level control — see
        // `crate::admin`'s own module doc comment for why this is gated on
        // a separate `AVALON_ADMIN_TOKEN`, not the public posture every
        // other `/nodes/*` route above takes.
        .route(
            "/nodes/log-level",
            get(admin::get_log_level).post(admin::set_log_level),
        )
        // Issue #539: one-hop live realtime event relay across nodes,
        // built on the peer table above — see `crate::realtime_relay`.
        .route("/nodes/relay", post(realtime_relay::relay_handler))
        // Issue #596: push-based mirror-sync notification — see
        // `crate::mirror_push`. No auth, same public posture as the
        // `/ledger/*` block above: the body is never trusted for anything
        // beyond waking this node's own mirror-watcher loop early.
        .route("/mirror/notify", post(mirror_push::notify))
        // Issue #540: async at-rest chat/conversation replication — see
        // `crate::chat_replication`.
        .route(
            "/nodes/replicate-chat",
            post(chat_replication::replicate_chat_handler),
        )
        // Issue #661: operator-internal, node-to-node RPC (epic #291's
        // Node Role Separation) — see `crate::internal_role`'s own module
        // doc comment for why this is a *third*, deliberately distinct
        // auth domain from both `/ledger/*` (issue #40, cross-operator)
        // and `/nodes/log-level` (this operator's own admin console)
        // above. `apply_indexer_event`/`rebuild_indexer` are the first,
        // proven instance of the pattern — the `Indexer` role over HTTP.
        .route(
            "/internal/indexer/apply",
            post(internal_role::apply_indexer_event),
        )
        .route(
            "/internal/indexer/rebuild",
            post(internal_role::rebuild_indexer),
        )
        .with_state(state)
        // Issue #265: every HTTP request gets a tracing span
        // (method/path/status/latency), and any `tracing::info!`/`error!`
        // call made while handling it is automatically correlated to that
        // span — this is what makes request-scoped log correlation work
        // without hand-threading a request id through every handler.
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer_from_env());

    // Issue #545: `AVALON_REDIS_URL` swaps both resource-limit layers for
    // their Redis-backed equivalents (`crate::redis_limits`) — per-hoster
    // shared state across that operator's own processes, never network-
    // wide. Unset (the default), the in-process layers below are
    // unchanged from #363.
    match redis_limiter {
        Some(limiter) => router
            // Concurrency first, same relative order the in-process
            // layers already use below.
            .layer(axum::middleware::from_fn_with_state(
                limiter.clone(),
                redis_limits::concurrency_middleware,
            ))
            .layer(axum::middleware::from_fn_with_state(
                limiter,
                redis_limits::rate_limit_middleware,
            )),
        None => router
            // Issue #363: concurrency backpressures (bounded wait), never
            // silently drops a request without a response.
            .layer(ConcurrencyLimitLayer::new(max_concurrent_requests))
            // Issue #363: per-key GCRA rate limit, 429 + Retry-After past
            // the configured ceiling — see `IntegratorOrIpKeyExtractor`.
            .layer(GovernorLayer::new(governor_config)),
    }
}
