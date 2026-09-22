//! Issue #723 (epic #722, decision #714): the generated OpenAPI schema for
//! `server`'s SDK-facing API surface (identity/auth, profile/presence,
//! social graph, chat, devices/passkeys/recovery, guilds, integrator/
//! achievements/registry) — deliberately excludes `/ledger/*`, `/nodes/*`,
//! `/mirror/*`, `/internal/*` (node/mirror infra, no SDK wraps these).
//!
//! Generated from the real handler signatures/types via
//! `#[utoipa::path]`/`#[derive(ToSchema)]` on each in-scope handler, not
//! hand-written — the whole point per #714's decision. `make openapi`
//! regenerates `docs/generated/openapi.json`; `make openapi-check` (part
//! of `make check`) fails CI if that file is stale relative to this
//! module's annotations. This module's own `paths(...)` list staying in
//! sync with `full_routes()`'s real route table over time is #728's job
//! (an automated route-table-vs-schema coverage check), not duplicated
//! here.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Avalon Protocol API",
        description = "SDK-facing API surface: identity/auth, profile/presence, social graph, chat, devices/passkeys/recovery, guilds, and the integrator/achievements/registry surface. Node/ledger/mirror/internal infrastructure routes are out of scope — no SDK wraps them.",
        version = "0.1.0"
    ),
    paths(
        crate::handlers::register_start,
        crate::handlers::register_finish,
        crate::handlers::session_start,
        crate::handlers::session_finish,
        crate::handlers::me,
        crate::handlers::update_profile,
        crate::handlers::my_history,
        crate::handlers::list_profiles,
        crate::handlers::get_identity_profile,
    ),
    components(schemas(
        crate::handlers::RegisterStartRequest,
        crate::handlers::RegisterStartResponse,
        crate::handlers::RegisterFinishRequest,
        crate::handlers::RegisterFinishResponse,
        crate::handlers::SessionStartRequest,
        crate::handlers::SessionStartResponse,
        crate::handlers::SessionFinishRequest,
        crate::handlers::SessionFinishResponse,
        crate::handlers::ProfileResponse,
        crate::handlers::PublicProfileResponse,
        crate::handlers::PublicIdentityProfileResponse,
        crate::handlers::HistoryEntryResponse,
        crate::handlers::UpdateProfileRequest,
    ))
)]
pub struct ApiDoc;
