use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("identity id already taken")]
    IdentityIdTaken,
    #[error("webauthn ceremony not found or already used")]
    CeremonyNotFound,
    #[error("webauthn ceremony expired")]
    CeremonyExpired,
    #[error("webauthn verification failed")]
    WebauthnFailed,
    #[error("event signature verification failed")]
    InvalidEventSignature,
    #[error("identity not found")]
    IdentityNotFound,
    #[error("cannot friend yourself")]
    SelfFriendRequest,
    #[error("already friends")]
    AlreadyFriends,
    #[error("a friend request is already pending")]
    FriendRequestExists,
    #[error("friend request not found")]
    FriendRequestNotFound,
    #[error("not friends")]
    NotFriends,
    #[error("invalid presence query")]
    InvalidPresenceQuery,
    #[error("a game may only publish presence claiming to be its own game id")]
    PresencePlayingMismatch,
    #[error("invalid profile query")]
    InvalidProfileQuery,
    #[error("no profile matches that handle")]
    HandleNotFound,
    #[error("could not generate a unique handle, try a different display name")]
    HandleGenerationFailed,
    #[error("avatar_url must be an http(s) URL of 2048 characters or fewer")]
    InvalidAvatarUrl,
    #[error("bio must be 500 characters or fewer")]
    InvalidBio,
    #[error("pronouns must be 40 characters or fewer")]
    InvalidPronouns,
    #[error("favorite_genres contains a value outside the fixed genre vocabulary")]
    InvalidGenre,
    #[error("favorite_genres may contain at most 5 entries")]
    TooManyFavoriteGenres,
    #[error("cannot block yourself")]
    SelfBlock,
    #[error("already blocked")]
    AlreadyBlocked,
    #[error("not blocked")]
    BlockNotFound,
    #[error("signing key not found")]
    SigningKeyNotFound,
    #[error("device grant not found or already resolved")]
    DeviceGrantNotFound,
    #[error("device grant has expired")]
    DeviceGrantExpired,
    #[error("approving signing key is unknown or has been revoked")]
    ApproverKeyInvalid,
    #[error("grant approval signature verification failed")]
    InvalidGrantSignature,
    #[error("guild not found")]
    GuildNotFound,
    #[error("guild name is already taken")]
    GuildNameTaken,
    #[error("guild tag is already taken")]
    GuildTagTaken,
    #[error("guild tag must be 2-5 characters")]
    InvalidGuildTag,
    #[error("guild role not found")]
    GuildRoleNotFound,
    #[error("role description must be 200 characters or fewer")]
    InvalidRoleDescription,
    #[error("unrecognized role badge icon or color")]
    InvalidRoleBadge,
    #[error("motd must be 500 characters or fewer")]
    InvalidGuildMotd,
    #[error("banner must be an http(s) URL of 2048 characters or fewer")]
    InvalidGuildBanner,
    #[error("links may contain at most 5 entries")]
    TooManyGuildLinks,
    #[error("each link's label must be 1-60 characters and url must be an http(s) URL of 2048 characters or fewer")]
    InvalidGuildLink,
    #[error("cannot change the owner role's permissions")]
    CannotModifyOwnerRole,
    #[error("missing required guild permission")]
    MissingGuildPermission,
    #[error("target identity is already the guild owner")]
    AlreadyGuildOwner,
    #[error("guild invite not found")]
    GuildInviteNotFound,
    #[error("identity is already a guild member")]
    AlreadyGuildMember,
    #[error("this guild is invite-only")]
    GuildNotOpen,
    #[error("not a guild member")]
    NotGuildMember,
    #[error("the guild owner must transfer ownership before leaving")]
    OwnerMustTransferBeforeLeaving,
    #[error("the guild owner cannot be removed")]
    CannotRemoveOwner,
    #[error("cannot assign the owner role through this endpoint")]
    CannotAssignOwnerRole,
    #[error("guild channel not found")]
    ChannelNotFound,
    #[error("channel name must be 1-100 characters")]
    InvalidChannelName,
    #[error("guild channel is archived")]
    ChannelArchived,
    #[error("message body must be non-empty and 4000 characters or fewer")]
    MessageTooLong,
    #[error("message not found")]
    MessageNotFound,
    #[error("game slug must be lowercase and match [a-z0-9-], 2-64 characters")]
    InvalidGameSlug,
    #[error("game slug is already taken")]
    GameSlugTaken,
    #[error("unsupported or invalid initial signing key")]
    InvalidGameKey,
    #[error("game not found")]
    GameNotFound,
    #[error("game challenge not found or already used")]
    GameChallengeNotFound,
    #[error("game challenge has expired")]
    GameChallengeExpired,
    #[error("game signing key not found")]
    GameKeyNotFound,
    #[error("game signature verification failed")]
    InvalidGameSignature,
    #[error("requested capability was not declared by the game at registration")]
    CapabilityNotRequested,
    #[error("no active binding to this game")]
    BindingNotFound,
    #[error("no active grant for that capability")]
    GrantNotFound,
    #[error("forbidden")]
    Forbidden,
    #[error("achievement key must be lowercase and match [a-z0-9_]+, 2-128 characters")]
    InvalidAchievementKey,
    #[error("an achievement definition with this key already exists for this game")]
    AchievementKeyTaken,
    #[error("achievement definition not found")]
    AchievementDefinitionNotFound,
    #[error("a game may only create or change its own achievement definitions")]
    AchievementDefinitionForbidden,
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("ledger error")]
    Ledger(#[from] avalon_chain::SettlementError),
    #[error("index error")]
    Index(#[from] avalon_indexer::IndexError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::IdentityIdTaken => StatusCode::CONFLICT,
            AppError::CeremonyNotFound | AppError::CeremonyExpired => StatusCode::BAD_REQUEST,
            AppError::WebauthnFailed | AppError::InvalidEventSignature => StatusCode::UNAUTHORIZED,
            AppError::IdentityNotFound | AppError::FriendRequestNotFound => StatusCode::NOT_FOUND,
            AppError::SelfFriendRequest
            | AppError::InvalidPresenceQuery
            | AppError::InvalidProfileQuery => StatusCode::BAD_REQUEST,
            // A game authenticated fine and even holds `presence.publish`
            // under an active binding — this is a distinct, narrower
            // rejection than `Forbidden` below: the one thing a capability
            // grant can never authorize is claiming to be a *different*
            // game. Still a 403: the request is well-formed, the caller
            // just isn't allowed to make this particular claim.
            AppError::PresencePlayingMismatch => StatusCode::FORBIDDEN,
            AppError::AlreadyFriends | AppError::FriendRequestExists => StatusCode::CONFLICT,
            AppError::NotFriends => StatusCode::NOT_FOUND,
            AppError::HandleNotFound => StatusCode::NOT_FOUND,
            AppError::HandleGenerationFailed => StatusCode::CONFLICT,
            AppError::InvalidAvatarUrl
            | AppError::InvalidBio
            | AppError::InvalidPronouns
            | AppError::InvalidGenre
            | AppError::TooManyFavoriteGenres => StatusCode::BAD_REQUEST,
            AppError::SelfBlock => StatusCode::BAD_REQUEST,
            AppError::AlreadyBlocked => StatusCode::CONFLICT,
            AppError::BlockNotFound => StatusCode::NOT_FOUND,
            AppError::SigningKeyNotFound | AppError::DeviceGrantNotFound => StatusCode::NOT_FOUND,
            AppError::DeviceGrantExpired => StatusCode::GONE,
            AppError::ApproverKeyInvalid | AppError::InvalidGrantSignature => {
                StatusCode::UNAUTHORIZED
            }
            AppError::GuildNotFound | AppError::GuildRoleNotFound => StatusCode::NOT_FOUND,
            AppError::GuildNameTaken | AppError::GuildTagTaken | AppError::AlreadyGuildOwner => {
                StatusCode::CONFLICT
            }
            AppError::InvalidGuildTag
            | AppError::InvalidRoleDescription
            | AppError::InvalidRoleBadge
            | AppError::InvalidGuildMotd
            | AppError::InvalidGuildBanner
            | AppError::TooManyGuildLinks
            | AppError::InvalidGuildLink => StatusCode::BAD_REQUEST,
            AppError::CannotModifyOwnerRole | AppError::MissingGuildPermission => {
                StatusCode::FORBIDDEN
            }
            AppError::GuildInviteNotFound => StatusCode::NOT_FOUND,
            AppError::AlreadyGuildMember => StatusCode::CONFLICT,
            AppError::GuildNotOpen => StatusCode::FORBIDDEN,
            AppError::OwnerMustTransferBeforeLeaving
            | AppError::CannotRemoveOwner
            | AppError::CannotAssignOwnerRole => StatusCode::FORBIDDEN,
            // "Not a guild member" is an authorization fact, not a missing
            // resource — the guild exists, the caller just isn't in it —
            // so this is 403 everywhere it's used, whether that's a
            // membership-gated read/write (#22) or a row-not-affected
            // check on a leave/remove/role-change mutation (#21).
            AppError::NotGuildMember => StatusCode::FORBIDDEN,
            AppError::ChannelNotFound | AppError::MessageNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidChannelName | AppError::MessageTooLong => StatusCode::BAD_REQUEST,
            AppError::ChannelArchived => StatusCode::CONFLICT,
            AppError::InvalidGameSlug | AppError::InvalidGameKey => StatusCode::BAD_REQUEST,
            AppError::GameSlugTaken => StatusCode::CONFLICT,
            AppError::GameNotFound => StatusCode::NOT_FOUND,
            // Auth-failure reasons for the game challenge-response scheme
            // (#26) all collapse to 401, same as `WebauthnFailed`/
            // `InvalidEventSignature` above — the specific reason is useful
            // for a legitimate caller debugging its own integration, not
            // something worth a different status code for.
            AppError::GameChallengeNotFound
            | AppError::GameChallengeExpired
            | AppError::GameKeyNotFound
            | AppError::InvalidGameSignature => StatusCode::UNAUTHORIZED,
            AppError::CapabilityNotRequested => StatusCode::BAD_REQUEST,
            AppError::BindingNotFound | AppError::GrantNotFound => StatusCode::NOT_FOUND,
            // Issue #28's guard: authenticated (as *some* game), but that
            // game lacks the specific capability it needs for this
            // request. Generic 403 body, same as every other authorization
            // (as opposed to authentication) failure in this file — see
            // `require_capability`'s own doc comment for why the body
            // never says *which* of "no binding" / "wrong game" / "no
            // grant" / "revoked grant" applied.
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::InvalidAchievementKey => StatusCode::BAD_REQUEST,
            AppError::AchievementKeyTaken => StatusCode::CONFLICT,
            AppError::AchievementDefinitionNotFound => StatusCode::NOT_FOUND,
            AppError::AchievementDefinitionForbidden => StatusCode::FORBIDDEN,
            AppError::Database(_) | AppError::Ledger(_) | AppError::Index(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        // Never leak internal error detail (e.g. SQL error text) to the client —
        // log it server-side once real observability exists; for now the
        // generic message is the safe default.
        let message = match &self {
            AppError::Database(_) | AppError::Ledger(_) | AppError::Index(_) => {
                "internal server error".to_string()
            }
            other => other.to_string(),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
