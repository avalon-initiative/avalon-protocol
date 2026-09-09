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
    #[error("passkey not found")]
    PasskeyNotFound,
    #[error("revoking your last remaining passkey requires ?confirm=true — this may lock you out if you have no other way to sign in")]
    LastPasskeyRequiresConfirmation,
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
    #[error("a role with that name already exists in this guild")]
    GuildRoleNameTaken,
    #[error("cannot delete the owner or member role")]
    CannotDeleteBaseRole,
    #[error("cannot delete a role while members still hold it")]
    RoleHasMembers,
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
    #[error("icon must be an http(s) URL of 2048 characters or fewer")]
    InvalidGuildIcon,
    #[error("join_policy must be \"invite_only\" or \"open\"")]
    InvalidJoinPolicy,
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
    #[error("guild event not found")]
    GuildEventNotFound,
    #[error("event title must be 1-200 characters")]
    InvalidEventTitle,
    #[error("event description must be 4000 characters or fewer")]
    InvalidEventDescription,
    #[error("event ends_at cannot be before starts_at")]
    InvalidEventTimeRange,
    #[error("rsvp status must be one of going, maybe, not_going")]
    InvalidRsvpStatus,
    #[error("resource_kind must be \"channel\" or \"event\"")]
    InvalidResourceKind,
    #[error("unrecognized guild permission")]
    InvalidPermission,
    #[error("permission override not found")]
    PermissionOverrideNotFound,
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
    #[error(
        "invalid guild discovery query: sort must be one of newest, alphabetical, most_members"
    )]
    InvalidDiscoverQuery,
    #[error("a guild may pin at most 5 favorite games")]
    TooManyFavoriteGames,
    #[error("favorite_games contains the same game more than once")]
    DuplicateFavoriteGame,
    #[error("a game can only be pinned as a favorite while it has at least one actively-bound guild member")]
    FavoriteGameNotBound,
    #[error("no signed tree head exists at that tree_size")]
    SignedTreeHeadNotFound,
    #[error("invalid proof query: seq/tree_size and first/second must be non-negative with the first bound not exceeding the second")]
    InvalidProofQuery,
    #[error("requested tree_size exceeds what has been committed to the ledger so far")]
    LedgerRangeNotCommitted,
    #[error("generated proof failed its own verification — refusing to return it")]
    ProofVerificationFailed,
    #[error("a game may only create or change its own achievement definitions")]
    AchievementDefinitionForbidden,
    #[error("guardian set must be 1-10 distinct friends (excluding yourself), with threshold between 1 and the guardian count")]
    InvalidGuardianSet,
    #[error("recovery is not configured for this identity")]
    RecoveryNotConfigured,
    /// Deliberately generic and used for both "this identity doesn't
    /// exist" and "this identity exists but has no guardians configured"
    /// on the unauthenticated `POST /recovery/requests/start`/`finish`
    /// endpoints (see `crate::recovery`'s module doc comment) — those two
    /// cases must be indistinguishable to an unauthenticated caller, or
    /// the endpoint becomes an oracle for enumerating which identity ids
    /// are real and which of those have recovery configured. Never use
    /// this for an authenticated-context lookup; `IdentityNotFound`/
    /// `RecoveryNotConfigured` above remain correct there, where
    /// distinguishing is fine.
    #[error("recovery is not available for this identity right now")]
    RecoveryNotAvailable,
    #[error("recovery request not found")]
    RecoveryRequestNotFound,
    #[error("a recovery attempt is already in progress for this identity")]
    RecoveryAlreadyInProgress,
    #[error("too many recovery attempts against this identity recently, try again later")]
    RecoveryRateLimited,
    #[error("this recovery request is not yet past its mandatory time-delay")]
    RecoveryNotReadyToFinalize,
    #[error("this recovery request has already been completed or cancelled")]
    RecoveryAlreadyResolved,
    #[error("caller is not a current guardian of this identity")]
    NotAGuardian,
    #[error("this guardian has already approved this recovery request")]
    AlreadyApproved,
    #[error("this guild is not currently recruiting")]
    GuildNotRecruiting,
    #[error("join request message must be 300 characters or fewer")]
    InvalidJoinRequestMessage,
    #[error("guild join request not found")]
    GuildJoinRequestNotFound,
    #[error("proto_source must be non-empty")]
    InvalidGameSchema,
    #[error("published schema version not found")]
    GameSchemaNotFound,
    #[error("a game may only publish schemas attributed to its own id")]
    GameSchemaForbidden,
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
            AppError::SigningKeyNotFound
            | AppError::DeviceGrantNotFound
            | AppError::PasskeyNotFound => StatusCode::NOT_FOUND,
            AppError::LastPasskeyRequiresConfirmation => StatusCode::CONFLICT,
            AppError::DeviceGrantExpired => StatusCode::GONE,
            AppError::ApproverKeyInvalid | AppError::InvalidGrantSignature => {
                StatusCode::UNAUTHORIZED
            }
            AppError::GuildNotFound | AppError::GuildRoleNotFound => StatusCode::NOT_FOUND,
            AppError::GuildNameTaken
            | AppError::GuildTagTaken
            | AppError::AlreadyGuildOwner
            | AppError::GuildRoleNameTaken
            | AppError::RoleHasMembers => StatusCode::CONFLICT,
            AppError::CannotDeleteBaseRole => StatusCode::FORBIDDEN,
            AppError::InvalidGuildTag
            | AppError::InvalidRoleDescription
            | AppError::InvalidRoleBadge
            | AppError::InvalidGuildMotd
            | AppError::InvalidGuildBanner
            | AppError::InvalidGuildIcon
            | AppError::InvalidJoinPolicy
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
            AppError::GuildEventNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidEventTitle
            | AppError::InvalidEventDescription
            | AppError::InvalidEventTimeRange
            | AppError::InvalidRsvpStatus
            | AppError::InvalidResourceKind
            | AppError::InvalidPermission => StatusCode::BAD_REQUEST,
            AppError::PermissionOverrideNotFound => StatusCode::NOT_FOUND,
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
            AppError::InvalidGuardianSet => StatusCode::BAD_REQUEST,
            AppError::RecoveryNotConfigured => StatusCode::CONFLICT,
            // Same status AND same body as a genuinely nonexistent
            // identity would get from this endpoint — see the variant's
            // own doc comment for why the two must be indistinguishable
            // here specifically.
            AppError::RecoveryNotAvailable => StatusCode::NOT_FOUND,
            AppError::RecoveryRequestNotFound => StatusCode::NOT_FOUND,
            AppError::RecoveryAlreadyInProgress => StatusCode::CONFLICT,
            AppError::RecoveryRateLimited => StatusCode::TOO_MANY_REQUESTS,
            AppError::RecoveryNotReadyToFinalize => StatusCode::CONFLICT,
            AppError::RecoveryAlreadyResolved => StatusCode::CONFLICT,
            AppError::NotAGuardian => StatusCode::FORBIDDEN,
            AppError::AlreadyApproved => StatusCode::CONFLICT,
            AppError::GuildNotRecruiting => StatusCode::FORBIDDEN,
            AppError::InvalidJoinRequestMessage => StatusCode::BAD_REQUEST,
            AppError::GuildJoinRequestNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidDiscoverQuery => StatusCode::BAD_REQUEST,
            AppError::InvalidGameSchema => StatusCode::BAD_REQUEST,
            AppError::GameSchemaNotFound => StatusCode::NOT_FOUND,
            // Same shape as `AchievementDefinitionForbidden`: authenticated
            // fine as *some* game, but that game isn't the one named by
            // the `{slug}` path segment — never allowed to publish a
            // schema attributed to another game's id.
            AppError::GameSchemaForbidden => StatusCode::FORBIDDEN,
            AppError::TooManyFavoriteGames | AppError::DuplicateFavoriteGame => {
                StatusCode::BAD_REQUEST
            }
            // The affinity this would-be pin claims doesn't currently exist
            // (zero actively-bound members) — a well-formed request the
            // caller isn't allowed to make, same "authenticated fine, just
            // not allowed to claim this" shape as `PresencePlayingMismatch`
            // above, not a 404 (the game itself may well exist).
            AppError::FavoriteGameNotBound => StatusCode::FORBIDDEN,
            AppError::SignedTreeHeadNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidProofQuery => StatusCode::BAD_REQUEST,
            // "Doesn't exist *yet*," not "never will" — a request for a
            // seq/tree_size beyond what's actually committed so far. 404,
            // matching every other not-found in this file, and explicitly
            // never a fabricated/empty proof (issue #211's own invariant).
            AppError::LedgerRangeNotCommitted => StatusCode::NOT_FOUND,
            // Should never actually happen — every proof this server
            // returns is self-verified before the response is built (see
            // `crate::settlement`). If it ever does, that's an internal bug
            // in proof generation, not a client-input problem.
            AppError::ProofVerificationFailed => StatusCode::INTERNAL_SERVER_ERROR,
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
            AppError::ProofVerificationFailed => "internal server error".to_string(),
            other => other.to_string(),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
