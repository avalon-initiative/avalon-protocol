//! Generates wire-shape request/response types for the
//! `crates/sdk/src/account/*` API surface from `docs/generated/openapi.json`
//! (#723), via `typify` (JSON Schema -> Rust types). Output lands in
//! `OUT_DIR` and is pulled in by `src/generated.rs`'s `include!`.
//!
//! `SCHEMA_NAMES` below is a deliberate allowlist, not "generate
//! everything" — scoped to the account/auth domain issue #724 covers, not
//! the rest of the API (guild-integrator surface, achievements, etc. stay
//! on their own hand-written types in `crate::guilds`/`crate::achievements`/
//! etc., outside this migration's scope). A handful of schemas are
//! deliberately excluded or replaced rather than generated verbatim:
//!
//! - Four WebAuthn-ceremony schemas (`RegisterStartResponse`,
//!   `RegisterFinishRequest`, `SessionStartResponse`, `SessionFinishRequest`)
//!   stay hand-written in `account/mod.rs`: their `challenge`/`credential`/
//!   `webauthn_credential` fields are opaque `"type": "object"` blobs in the
//!   schema (since `webauthn-rs`'s own types have no `ToSchema` impl), and
//!   typify would flatten them to `serde_json::Value`, discarding the real
//!   `passkey_types::webauthn::*` typing the ceremony code needs. Same
//!   reasoning covers `AddPasskeyStartResponse`/`AddPasskeyFinishRequest` in
//!   `passkeys.rs`.
//! - Pure signature-only wrapper types (just `signing_key_id`/`signature`,
//!   flattened via `super::SignatureFields` into a real payload elsewhere —
//!   `RevokePasskeyRequest`, `ApprovePairingRequest`'s payload-bearing
//!   variant aside, `ConnectRequest`) stay hand-written where the
//!   hand-written `SignatureFields` composition is the DRY choice; every
//!   *other* signature-carrying request (role/override/member/ownership
//!   mutations, `ApprovePairingRequest`, `SetGuardiansRequest`, ...) is
//!   generated, with `signature`/`signing_key_id` destructured out of
//!   `SignatureFields` at each call site instead of `#[serde(flatten)]`.
//! - `Genre`, `PresenceStatus`, `GuildLink`, `RoleBadge` are replaced with
//!   the existing `avalon_protocol::{identity,social,guilds}::*` types they
//!   already correspond to, rather than generating duplicates.
//!
//! Known, pre-existing gaps surfaced by comparing hand-written types against
//! the real generated schemas (server response/request shapes the
//! hand-written SDK never fully caught up to) are called out at their
//! struct definitions in `crates/sdk/src/account/*.rs`, not silently
//! expanded or silently dropped as part of this migration.
//!
//! `sdk` reads `openapi.json` as a plain checked-in data file, not a Cargo
//! build-graph output — `crates/server` already depends on `avalon-sdk`,
//! so `sdk` depending on `avalon-server` (or invoking its `dump-openapi`
//! binary) would create a workspace cycle.

use std::env;
use std::fs;
use std::path::Path;

use schemars::schema::Schema;
use typify::{TypeSpace, TypeSpaceSettings};

const SCHEMA_NAMES: &[&str] = &[
    "ProfileResponse",
    "DeviceResponse",
    "RegisterStartRequest",
    "RegisterFinishResponse",
    "SessionStartRequest",
    "SessionFinishResponse",
    "UpdateProfileRequest",
    "PasskeyResponse",
    "RenamePasskeyRequest",
    "ConversationResponse",
    "CreateConversationRequest",
    "MessageResponse",
    "ConversationMessageResponse",
    "SendMessageRequest",
    "ConnectResponse",
    "Connection",
    "ConnectionGrant",
    "DeviceGrantResponse",
    "RequestDeviceGrantRequest",
    "ApproveDeviceGrantRequest",
    "RenameDeviceRequest",
    "ApprovePairingRequest",
    "UserCodeRequest",
    "ResolvePairingResponse",
    "CancelRecoveryRequest",
    "GuardianOfSummary",
    "GuardianRequestSummary",
    "GuardianSettingsResponse",
    "RecoveryRequestResponse",
    "SetGuardiansRequest",
    "FriendshipResponse",
    "FriendRequestResponse",
    "BlockResponse",
    "BlockListEntry",
    "PublicProfileResponse",
    "DiscoveryCandidate",
    "SearchResultIdentity",
    "PresenceResponse",
    "HistoryEntryResponse",
    "PublicIdentityProfileResponse",
    "GuildAnnouncementAlert",
    "DiscoverPeopleResponse",
    "SearchIdentitiesResponse",
    "ResolveHandleResponse",
    "CreateFriendRequestRequest",
    "CreateBlockRequest",
    "UpdatePresenceRequest",
    "GuildResponse",
    "DiscoverGuildSummary",
    "DiscoverGuildsResponse",
    "FavoriteGameEntry",
    "FavoriteGamesResponse",
    "RoleResponse",
    "PermissionOverrideResponse",
    "GuildMemberResponse",
    "MyGuildMembershipResponse",
    "GuildInviteResponse",
    "MyGuildInviteResponse",
    "GuildJoinRequestResponse",
    "ChannelResponse",
    "RsvpCounts",
    "EventResponse",
    "RsvpResponse",
    "RsvpRosterEntry",
    "CreateGuildRequest",
    "UpdateGuildRequest",
    "CreateRoleRequest",
    "UpdateRoleRequest",
    "DeleteRoleRequest",
    "SetPermissionOverrideRequest",
    "DeletePermissionOverrideRequest",
    "UpdateGuildMemberRequest",
    "CreateGuildInviteRequest",
    "CreateChannelRequest",
    "UpdateChannelRequest",
    "CreateEventRequest",
    "UpdateEventRequest",
    "RsvpRequest",
    "SetFavoriteGamesRequest",
    "TransferOwnershipRequest",
    "CreateJoinRequestRequest",
    "StartPairingResponse",
    "PollPairingResponse",
    "RoleBadgeRequest",
    "GuildLinkRequest",
    // Pulled in transitively by ProfileResponse.favorite_genres — replaced
    // below with the existing `avalon_protocol::identity::Genre` rather
    // than generated fresh, since that's already the real type
    // `avalon_protocol::identity::Profile` (and its callers) uses.
    "Genre",
    // Pulled in transitively by PresenceResponse/UpdatePresenceRequest —
    // replaced below with the existing `avalon_protocol::social::PresenceStatus`.
    "PresenceStatus",
    // Pulled in transitively by GuildResponse.links — replaced below with
    // the existing `avalon_protocol::guilds::GuildLink`.
    "GuildLink",
    // Pulled in transitively by RoleResponse.badge — replaced below with
    // the existing `avalon_protocol::guilds::RoleBadge`.
    "RoleBadge",
];

/// Typify hardcodes `"format": "date-time"` to `chrono::DateTime<Utc>`,
/// with no setting to redirect it to the `time` crate this workspace
/// actually uses everywhere else (`Cargo.toml` has no `chrono` dependency
/// at all — adding one just for this would duplicate date/time handling
/// crate-wide for three fields). Instead, strip the `date-time` format
/// hint before handing schemas to typify, so those fields come through as
/// plain `String`, and parse them with `time`'s own RFC3339 support at the
/// handful of call sites that read them (see `account/mod.rs`).
fn strip_date_time_format(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("format").and_then(|f| f.as_str()) == Some("date-time") {
                map.remove("format");
            }
            for v in map.values_mut() {
                strip_date_time_format(v);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                strip_date_time_format(v);
            }
        }
        _ => {}
    }
}

fn main() {
    let openapi_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/generated/openapi.json");
    println!("cargo:rerun-if-changed={}", openapi_path.display());

    let raw = fs::read_to_string(&openapi_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", openapi_path.display()));
    let doc: serde_json::Value =
        serde_json::from_str(&raw).expect("docs/generated/openapi.json is valid JSON");
    let schemas = doc["components"]["schemas"]
        .as_object()
        .expect("docs/generated/openapi.json has components.schemas");

    let defs: Vec<(String, Schema)> = SCHEMA_NAMES
        .iter()
        .map(|name| {
            let mut value = schemas
                .get(*name)
                .unwrap_or_else(|| {
                    panic!(
                        "docs/generated/openapi.json is missing components.schemas.{name} \
                         — update crates/sdk/build.rs's SCHEMA_NAMES or check for a schema rename"
                    )
                })
                .clone();
            strip_date_time_format(&mut value);
            let schema: Schema = serde_json::from_value(value).unwrap_or_else(|e| {
                panic!("components.schemas.{name} did not parse as a JSON Schema: {e}")
            });
            (name.to_string(), schema)
        })
        .collect();

    let mut settings = TypeSpaceSettings::default();
    settings.with_replacement(
        "Genre",
        "avalon_protocol::identity::Genre",
        std::iter::empty(),
    );
    settings.with_replacement(
        "PresenceStatus",
        "avalon_protocol::social::PresenceStatus",
        std::iter::empty(),
    );
    settings.with_replacement(
        "GuildLink",
        "avalon_protocol::guilds::GuildLink",
        std::iter::empty(),
    );
    settings.with_replacement(
        "RoleBadge",
        "avalon_protocol::guilds::RoleBadge",
        std::iter::empty(),
    );
    let mut type_space = TypeSpace::new(&settings);
    type_space
        .add_ref_types(defs)
        .expect("typify failed to convert the account/auth schema allowlist");

    let mut contents = type_space.to_stream().to_string();
    contents.push('\n');
    contents.push_str(&generate_path_stubs(&doc));

    let out_path = Path::new(&env::var("OUT_DIR").unwrap()).join("generated.rs");
    fs::write(&out_path, contents)
        .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
}

/// Endpoint-path stub constants — the other half of #724's original
/// design that the first migration pass left undone. One `&str` constant
/// per (tag, operationId) pair across the *whole* published API (not just
/// the account/auth `SCHEMA_NAMES` allowlist above — unlike type
/// generation, a path template carries no format-mapping risk, so there's
/// no reason to hand-curate a second allowlist in lockstep with the
/// first). `operationId` alone collides across tags (`list_messages`,
/// `send_message`, `register_start`, `register_finish` each appear under
/// two different tags for two different real endpoints) — confirmed via
/// `(tag, operationId)` being unique across the whole spec, so each tag
/// gets its own module and the constant name is bare `operationId`
/// (SCREAMING_SNAKE_CASE).
///
/// Path templates keep the server's own `{param}` placeholder names
/// (which don't always match this SDK's local variable names, e.g.
/// `/guilds/{id}/channels/{cid}` vs. `guild_id`/`channel_id`) — `format!()`
/// requires a string *literal*, so a runtime `&str` constant can't be fed
/// into it regardless of naming; call sites instead use
/// `account::path(TEMPLATE, &[("id", &guild_id.to_string()), ...])`, a
/// small runtime substitution helper (`crates/sdk/src/account/mod.rs`).
fn generate_path_stubs(doc: &serde_json::Value) -> String {
    let paths = doc["paths"]
        .as_object()
        .expect("docs/generated/openapi.json has paths");

    let mut by_tag: std::collections::BTreeMap<String, Vec<(String, String, String)>> =
        std::collections::BTreeMap::new();

    for (path, methods) in paths {
        let methods = methods.as_object().expect("path item is an object");
        for (method, op) in methods {
            if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                continue;
            }
            let tag = op["tags"][0]
                .as_str()
                .unwrap_or("untagged")
                .replace('-', "_");
            let operation_id = op["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no operationId"))
                .to_string();
            by_tag.entry(tag).or_default().push((
                operation_id,
                method.to_uppercase(),
                path.clone(),
            ));
        }
    }

    let mut out = String::from("pub mod paths {\n");
    for (tag, mut ops) in by_tag {
        ops.sort();
        out.push_str(&format!("    pub mod {tag} {{\n"));
        for (operation_id, method, path) in ops {
            let const_name = operation_id.to_uppercase();
            out.push_str(&format!(
                "        /// `{method} {path}`\n        pub const {const_name}: &str = {path:?};\n"
            ));
        }
        out.push_str("    }\n");
    }
    out.push_str("}\n");
    out
}
