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
    #[error("an integrator may only publish presence claiming to be its own integrator id")]
    PresenceActiveInMismatch,
    #[error("invalid profile query")]
    InvalidProfileQuery,
    #[error("no profile matches that handle")]
    HandleNotFound,
    /// Issue #510: `display_name` (case-insensitive) is already held by a
    /// different identity — a hard rejection, never an auto-suggested
    /// variant. Raised both by `register_start`'s advisory pre-check and
    /// by mapping a real `IndexError::DisplayNameTaken` (the actual
    /// enforcement point) at `register_finish`/`update_profile`.
    #[error("display_name is already taken")]
    DisplayNameTaken,
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
    #[error("status must be 100 characters or fewer")]
    InvalidStatus,
    #[error("links may contain at most 5 entries")]
    TooManyLinks,
    #[error("each links entry must be an http(s) URL of 200 characters or fewer")]
    InvalidLink,
    #[error("timezone must be 1-64 characters")]
    InvalidTimezone,
    #[error("theme_color must be a 6-digit hex color, e.g. #a1b2c3")]
    InvalidThemeColor,
    #[error("location must be 100 characters or fewer")]
    InvalidLocation,
    #[error("main_guild must be a valid guild id")]
    InvalidMainGuild,
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
    #[error("no completed recovery exists for this identity")]
    RollbackNoCompletedRecovery,
    #[error(
        "rollback window is invalid: `since` must be a timestamp before the recovery completed"
    )]
    InvalidRollbackWindow,
    #[error("event is not eligible for rollback")]
    RollbackEventNotEligible,
    #[error("{0}")]
    RollbackNotReversible(String),
    #[error("event has already been reversed")]
    RollbackAlreadyReversed,
    #[error("device grant not found or already resolved")]
    DeviceGrantNotFound,
    #[error("device grant has expired")]
    DeviceGrantExpired,
    #[error("approving signing key is unknown or has been revoked")]
    ApproverKeyInvalid,
    #[error("grant approval signature verification failed")]
    InvalidGrantSignature,
    /// Issue #698: this action is in #697's signature-required tier, and
    /// the caller has no non-revoked `identity_signing_keys` row at all —
    /// distinct from [`Self::FreshSignatureRequired`] (has a key, just
    /// didn't sign this request) so the client gets an actionable message
    /// rather than a generic auth failure.
    #[error("this action requires a signature from a registered signing key, and this identity has none registered — see POST /me/devices/grants")]
    NoRegisteredSigningKey,
    /// Issue #698: the caller has at least one active signing key but
    /// omitted `signing_key_id`/`signature` (or sent an empty signature) on
    /// a request in #697's signature-required tier.
    #[error("this action requires a fresh signature from one of your registered signing keys")]
    FreshSignatureRequired,
    /// Issue #698: the supplied signature doesn't verify against the named
    /// signing key over this action's own canonical fields. `signing_key_id`
    /// not resolving to a non-revoked, caller-owned key reuses the existing
    /// [`Self::SigningKeyNotFound`] above rather than a new variant.
    #[error("fresh signature verification failed")]
    InvalidFreshSignature,
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
    #[error(
        "roster_visibility must be one of \"public\", \"authenticated_only\", \"friends\", \"guild_members\", \"private\""
    )]
    InvalidVisibility,
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
    #[error("channel topic must be 200 characters or fewer")]
    InvalidChannelTopic,
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
    #[error("integrator slug must be lowercase and match [a-z0-9-], 2-64 characters")]
    InvalidIntegratorSlug,
    #[error("integrator slug is already taken")]
    IntegratorSlugTaken,
    #[error("unsupported or invalid initial signing key")]
    InvalidIntegratorKey,
    #[error("category must be one of integrator, app, service")]
    InvalidIntegratorCategory,
    #[error("integrator not found")]
    IntegratorNotFound,
    #[error("invalid integrators list query: sort must be one of newest, name")]
    InvalidIntegratorsListQuery,
    #[error("invalid achievements list query: claim_kind must be one of achievement, milestone")]
    InvalidAchievementsListQuery,
    #[error("integrator challenge not found or already used")]
    IntegratorChallengeNotFound,
    #[error("integrator challenge has expired")]
    IntegratorChallengeExpired,
    #[error("integrator signing key not found")]
    IntegratorKeyNotFound,
    #[error("integrator signature verification failed")]
    InvalidIntegratorSignature,
    #[error("issuer registration challenge not found or already used")]
    IssuerRegistrationChallengeNotFound,
    #[error("issuer registration challenge has expired")]
    IssuerRegistrationChallengeExpired,
    #[error("proof-of-possession signature does not verify against the supplied issuer_pubkey")]
    InvalidProofOfPossession,
    #[error("declared_network_id does not match this server's own network")]
    DeclaredNetworkMismatch,
    #[error("this issuer key is not registered on this network — see POST /issuers/register")]
    IssuerNotRegisteredOnNetwork,
    #[error("this action requires the issuer's root key, not an operational key")]
    IssuerKeyNotRoot,
    #[error("key role must be one of root, operational")]
    InvalidIssuerKeyRole,
    #[error("key purpose must be one of attestation, shard_settlement")]
    InvalidIssuerKeyPurpose,
    #[error("issuer key not found, already revoked, or not owned by this integrator")]
    IssuerKeyForbidden,
    #[error("requested capability was not declared by the integrator at registration")]
    CapabilityNotRequested,
    #[error("no active binding to this integrator")]
    BindingNotFound,
    #[error("no active grant for that capability")]
    GrantNotFound,
    #[error("forbidden")]
    Forbidden,
    #[error("achievement key must be lowercase and match [a-z0-9_]+, 2-128 characters")]
    InvalidAchievementKey,
    #[error("an achievement definition with this key already exists for this integrator")]
    AchievementKeyTaken,
    #[error("achievement definition not found")]
    AchievementDefinitionNotFound,
    #[error("this issuer's registered category doesn't use this claim vocabulary (achievement vs. milestone) — see issue #324")]
    ClaimVocabularyMismatch,
    #[error("icon must be one of the built-in icon keys (trophy, star, shield, sword)")]
    InvalidAchievementIcon,
    #[error("icon_url must be an absolute http or https URL")]
    InvalidAchievementIconUrl,
    #[error("cannot issue against a retired definition")]
    AttestationDefinitionRetired,
    #[error(
        "attestation signature does not verify against any of the issuer's currently-valid keys"
    )]
    InvalidAttestationSignature,
    #[error("a bulk issuance call must carry between 1 and {max} claims", max = crate::achievements::MAX_BULK_CLAIMS)]
    InvalidBulkAttestationRequest,
    #[error("this issuer has written too many attestations about this subject recently")]
    AttestationWriteQuotaExceeded,
    #[error("attestation not found")]
    AttestationNotFound,
    #[error("only the issuer that issued this attestation may revoke it")]
    AttestationRevocationForbidden,
    #[error("this attestation has already been revoked")]
    AttestationAlreadyRevoked,
    #[error(
        "invalid guild discovery query: sort must be one of newest, alphabetical, most_members"
    )]
    InvalidDiscoverQuery,
    #[error("a guild may pin at most 5 favorite integrators")]
    TooManyFavoriteGames,
    #[error("favorite_games contains the same integrator more than once")]
    DuplicateFavoriteGame,
    #[error("an integrator can only be pinned as a favorite while it has at least one actively-bound guild member")]
    FavoriteGameNotBound,
    #[error("no signed tree head exists at that tree_size")]
    SignedTreeHeadNotFound,
    /// Issue #519: same "not found" as [`AppError::SignedTreeHeadNotFound`],
    /// but for a node that's configured as a mirror for the requested
    /// shard and simply hasn't backfilled anything from its peer yet —
    /// distinct from a genuine empty authority, whose 404 stays the plain
    /// variant above. Never fabricates or synthesizes an STH; this only
    /// changes what the error body says about *why* nothing was found.
    #[error("no signed tree head exists yet (mirror not yet backfilled)")]
    SignedTreeHeadNotFoundMirror { peers: Vec<String> },
    #[error("invalid proof query: seq/tree_size and first/second must be non-negative with the first bound not exceeding the second")]
    InvalidProofQuery,
    #[error("requested tree_size exceeds what has been committed to the ledger so far")]
    LedgerRangeNotCommitted,
    #[error("generated proof failed its own verification — refusing to return it")]
    ProofVerificationFailed,
    #[error("invalid entries query: since_seq must be non-negative and limit must be a positive integer")]
    InvalidEntriesQuery,
    #[error("an integrator may only create or change its own achievement definitions")]
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
    #[error("rate limit exceeded for this account, try again later")]
    PrincipalRateLimited { retry_after_secs: u64 },
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
    InvalidIntegratorSchema,
    #[error("published schema version not found")]
    IntegratorSchemaNotFound,
    #[error("an integrator may only publish schemas attributed to its own id")]
    IntegratorSchemaForbidden,
    #[error("invalid proto schema: {detail}")]
    InvalidProtoSchema { detail: String },
    #[error("instance does not conform to its schema: {detail}")]
    InstanceSchemaMismatch { detail: String },
    #[error("integrator data instance not found")]
    IntegratorDataInstanceNotFound,
    #[error("an integrator may only publish instance data against its own published schemas")]
    IntegratorDataSchemaOwnershipMismatch,
    #[error("a mapping's from/to must both be real, published schema version ids")]
    IntegratorSchemaMappingSchemaNotFound,
    #[error("an integrator may only publish a mapping between schema versions it owns")]
    IntegratorSchemaMappingSchemaOwnershipMismatch,
    #[error("a mapping's from and to must reference two distinct schema versions")]
    InvalidIntegratorSchemaMapping,
    #[error("published schema mapping not found")]
    IntegratorSchemaMappingNotFound,
    #[error("an integrator may only publish mappings attributed to its own id")]
    IntegratorSchemaMappingForbidden,
    #[error(
        "participants must include yourself plus at least one other distinct existing identity"
    )]
    InvalidConversationParticipants,
    /// Deliberately reused for two different reasons — see
    /// `crate::conversations`' module doc comment: the caller genuinely
    /// isn't a participant in this conversation, *or* they are a
    /// participant but a block exists between some pair of participants
    /// and the request was a send. Both cases return this exact same
    /// error, on purpose, so a blocked participant sending into a group
    /// conversation sees nothing that distinguishes "you're blocked" from
    /// "you were never in this conversation" — the same never-reveal
    /// posture issue #97 established for friend requests and presence.
    #[error("not a participant in this conversation")]
    NotConversationParticipant,
    #[error("an integrator may only publish or revoke its own recognition declarations")]
    IntegratorRecognitionForbidden,
    #[error("scope must be a non-empty list of non-empty strings, and an integrator cannot recognize itself")]
    InvalidRecognitionScope,
    #[error("device pairing not found, already resolved, or expired")]
    DevicePairingNotFound,
    #[error("failed to generate a unique pairing code, try again")]
    DevicePairingCodeGenerationFailed,
    #[error("cross-node login request not found, already resolved, or expired")]
    CrossNodeLoginRequestNotFound,
    #[error("failed to generate a unique cross-node login request code, try again")]
    CrossNodeLoginRequestCodeGenerationFailed,
    #[error("peer announced a different network_id than this node's own")]
    PeerNetworkMismatch,
    /// Issue #658: `body.filter` failed `tracing_subscriber::EnvFilter`'s
    /// own parse — the currently-active filter is left untouched.
    #[error("invalid log filter syntax")]
    InvalidLogFilter,
    /// Should never actually happen — the reload handle only errors if the
    /// subscriber it points at has already been dropped, which can't occur
    /// while this process is still up and serving requests. If it ever
    /// does, that's an internal bug, not a client-input problem — same
    /// "should never happen" posture `ProofVerificationFailed` already
    /// establishes.
    #[error("failed to read or apply the live log filter")]
    LogReloadFailed,
    /// Issue #661: an operator-internal role this node depends on (e.g. a
    /// remote Indexer over `crate::internal_role`'s protocol) didn't
    /// answer — a connection failure, timeout, or non-success response.
    /// Deliberately distinct from [`Self::Index`]/[`Self::Database`]: those
    /// mean the dependency *did* run and reported a real failure of its
    /// own; this means the dependency itself was unreachable, a condition
    /// callers and operators need to be able to tell apart from "the data
    /// doesn't exist" or "a storage bug" (see #661's own ticket). `role`
    /// names which internal role was unreachable (e.g. `"indexer"`),
    /// `detail` carries the underlying network error for logs/debugging —
    /// never surfaced verbatim as trusted content, just diagnostic text.
    #[error("remote role '{role}' is unreachable: {detail}")]
    RemoteRoleUnreachable { role: String, detail: String },
    /// Issue #629, implementing #622's decision: this shard has passed its
    /// bootstrap grace period and doesn't yet have enough
    /// independently-confirmed mirrors (`AVALON_MIN_MIRROR_CONFIRMATIONS`)
    /// to accept a *new* identity registration — see
    /// `crate::replication`'s own module doc comment. Only ever returned
    /// from `crate::handlers::register_start`; never affects an identity
    /// already registered on the shard.
    #[error("this shard does not yet have enough independently-confirmed mirrors to accept new registrations")]
    ShardBelowMinimumReplication,
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("ledger error")]
    Ledger(#[from] avalon_chain::SettlementError),
    /// Deliberately hand-written rather than `#[from]`: an
    /// [`avalon_indexer::IndexError::RemoteUnreachable`] must map to
    /// [`Self::RemoteRoleUnreachable`] (a distinct 503, not a generic
    /// 500) — see that variant's own doc comment.
    #[error("index error")]
    Index(avalon_indexer::IndexError),
}

impl From<avalon_indexer::IndexError> for AppError {
    fn from(err: avalon_indexer::IndexError) -> Self {
        match err {
            avalon_indexer::IndexError::RemoteUnreachable(detail) => {
                AppError::RemoteRoleUnreachable {
                    role: "indexer".to_string(),
                    detail,
                }
            }
            other => AppError::Index(other),
        }
    }
}

impl AppError {
    /// A stable, machine-readable identifier for this variant — one per
    /// variant, screaming-snake-case of its own Rust name, never the
    /// human-readable `{error}` message text (which stays free to reword
    /// without becoming a breaking change). The SDK maps on this, not on
    /// `{error}` or the HTTP status alone — several variants
    /// share a status code (e.g. many `NOT_FOUND`s), and message text isn't
    /// contractually stable.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "UNAUTHORIZED",
            AppError::IdentityIdTaken => "IDENTITY_ID_TAKEN",
            AppError::CeremonyNotFound => "CEREMONY_NOT_FOUND",
            AppError::CeremonyExpired => "CEREMONY_EXPIRED",
            AppError::WebauthnFailed => "WEBAUTHN_FAILED",
            AppError::InvalidEventSignature => "INVALID_EVENT_SIGNATURE",
            AppError::IdentityNotFound => "IDENTITY_NOT_FOUND",
            AppError::SelfFriendRequest => "SELF_FRIEND_REQUEST",
            AppError::AlreadyFriends => "ALREADY_FRIENDS",
            AppError::FriendRequestExists => "FRIEND_REQUEST_EXISTS",
            AppError::FriendRequestNotFound => "FRIEND_REQUEST_NOT_FOUND",
            AppError::NotFriends => "NOT_FRIENDS",
            AppError::InvalidPresenceQuery => "INVALID_PRESENCE_QUERY",
            AppError::PresenceActiveInMismatch => "PRESENCE_ACTIVE_IN_MISMATCH",
            AppError::InvalidProfileQuery => "INVALID_PROFILE_QUERY",
            AppError::HandleNotFound => "HANDLE_NOT_FOUND",
            AppError::DisplayNameTaken => "DISPLAY_NAME_TAKEN",
            AppError::InvalidAvatarUrl => "INVALID_AVATAR_URL",
            AppError::InvalidBio => "INVALID_BIO",
            AppError::InvalidPronouns => "INVALID_PRONOUNS",
            AppError::InvalidGenre => "INVALID_GENRE",
            AppError::TooManyFavoriteGenres => "TOO_MANY_FAVORITE_GENRES",
            AppError::InvalidStatus => "INVALID_STATUS",
            AppError::TooManyLinks => "TOO_MANY_LINKS",
            AppError::InvalidLink => "INVALID_LINK",
            AppError::InvalidTimezone => "INVALID_TIMEZONE",
            AppError::InvalidThemeColor => "INVALID_THEME_COLOR",
            AppError::InvalidLocation => "INVALID_LOCATION",
            AppError::InvalidMainGuild => "INVALID_MAIN_GUILD",
            AppError::SelfBlock => "SELF_BLOCK",
            AppError::AlreadyBlocked => "ALREADY_BLOCKED",
            AppError::BlockNotFound => "BLOCK_NOT_FOUND",
            AppError::SigningKeyNotFound => "SIGNING_KEY_NOT_FOUND",
            AppError::PasskeyNotFound => "PASSKEY_NOT_FOUND",
            AppError::RollbackNoCompletedRecovery => "ROLLBACK_NO_COMPLETED_RECOVERY",
            AppError::InvalidRollbackWindow => "INVALID_ROLLBACK_WINDOW",
            AppError::RollbackEventNotEligible => "ROLLBACK_EVENT_NOT_ELIGIBLE",
            AppError::RollbackNotReversible(_) => "ROLLBACK_NOT_REVERSIBLE",
            AppError::RollbackAlreadyReversed => "ROLLBACK_ALREADY_REVERSED",
            AppError::DeviceGrantNotFound => "DEVICE_GRANT_NOT_FOUND",
            AppError::DeviceGrantExpired => "DEVICE_GRANT_EXPIRED",
            AppError::ApproverKeyInvalid => "APPROVER_KEY_INVALID",
            AppError::InvalidGrantSignature => "INVALID_GRANT_SIGNATURE",
            AppError::NoRegisteredSigningKey => "NO_REGISTERED_SIGNING_KEY",
            AppError::FreshSignatureRequired => "FRESH_SIGNATURE_REQUIRED",
            AppError::InvalidFreshSignature => "INVALID_FRESH_SIGNATURE",
            AppError::GuildNotFound => "GUILD_NOT_FOUND",
            AppError::GuildNameTaken => "GUILD_NAME_TAKEN",
            AppError::GuildTagTaken => "GUILD_TAG_TAKEN",
            AppError::GuildRoleNameTaken => "GUILD_ROLE_NAME_TAKEN",
            AppError::CannotDeleteBaseRole => "CANNOT_DELETE_BASE_ROLE",
            AppError::RoleHasMembers => "ROLE_HAS_MEMBERS",
            AppError::InvalidGuildTag => "INVALID_GUILD_TAG",
            AppError::GuildRoleNotFound => "GUILD_ROLE_NOT_FOUND",
            AppError::InvalidRoleDescription => "INVALID_ROLE_DESCRIPTION",
            AppError::InvalidRoleBadge => "INVALID_ROLE_BADGE",
            AppError::InvalidGuildMotd => "INVALID_GUILD_MOTD",
            AppError::InvalidGuildBanner => "INVALID_GUILD_BANNER",
            AppError::InvalidGuildIcon => "INVALID_GUILD_ICON",
            AppError::InvalidJoinPolicy => "INVALID_JOIN_POLICY",
            AppError::InvalidVisibility => "INVALID_VISIBILITY",
            AppError::TooManyGuildLinks => "TOO_MANY_GUILD_LINKS",
            AppError::InvalidGuildLink => "INVALID_GUILD_LINK",
            AppError::CannotModifyOwnerRole => "CANNOT_MODIFY_OWNER_ROLE",
            AppError::MissingGuildPermission => "MISSING_GUILD_PERMISSION",
            AppError::AlreadyGuildOwner => "ALREADY_GUILD_OWNER",
            AppError::GuildInviteNotFound => "GUILD_INVITE_NOT_FOUND",
            AppError::AlreadyGuildMember => "ALREADY_GUILD_MEMBER",
            AppError::GuildNotOpen => "GUILD_NOT_OPEN",
            AppError::NotGuildMember => "NOT_GUILD_MEMBER",
            AppError::OwnerMustTransferBeforeLeaving => "OWNER_MUST_TRANSFER_BEFORE_LEAVING",
            AppError::CannotRemoveOwner => "CANNOT_REMOVE_OWNER",
            AppError::CannotAssignOwnerRole => "CANNOT_ASSIGN_OWNER_ROLE",
            AppError::ChannelNotFound => "CHANNEL_NOT_FOUND",
            AppError::InvalidChannelName => "INVALID_CHANNEL_NAME",
            AppError::InvalidChannelTopic => "INVALID_CHANNEL_TOPIC",
            AppError::ChannelArchived => "CHANNEL_ARCHIVED",
            AppError::MessageTooLong => "MESSAGE_TOO_LONG",
            AppError::MessageNotFound => "MESSAGE_NOT_FOUND",
            AppError::GuildEventNotFound => "GUILD_EVENT_NOT_FOUND",
            AppError::InvalidEventTitle => "INVALID_EVENT_TITLE",
            AppError::InvalidEventDescription => "INVALID_EVENT_DESCRIPTION",
            AppError::InvalidEventTimeRange => "INVALID_EVENT_TIME_RANGE",
            AppError::InvalidRsvpStatus => "INVALID_RSVP_STATUS",
            AppError::InvalidResourceKind => "INVALID_RESOURCE_KIND",
            AppError::InvalidPermission => "INVALID_PERMISSION",
            AppError::PermissionOverrideNotFound => "PERMISSION_OVERRIDE_NOT_FOUND",
            AppError::InvalidIntegratorSlug => "INVALID_INTEGRATOR_SLUG",
            AppError::IntegratorSlugTaken => "INTEGRATOR_SLUG_TAKEN",
            AppError::InvalidIntegratorKey => "INVALID_INTEGRATOR_KEY",
            AppError::InvalidIntegratorCategory => "INVALID_INTEGRATOR_CATEGORY",
            AppError::IntegratorNotFound => "INTEGRATOR_NOT_FOUND",
            AppError::InvalidIntegratorsListQuery => "INVALID_INTEGRATORS_LIST_QUERY",
            AppError::InvalidAchievementsListQuery => "INVALID_ACHIEVEMENTS_LIST_QUERY",
            AppError::IntegratorChallengeNotFound => "INTEGRATOR_CHALLENGE_NOT_FOUND",
            AppError::IntegratorChallengeExpired => "INTEGRATOR_CHALLENGE_EXPIRED",
            AppError::IntegratorKeyNotFound => "INTEGRATOR_KEY_NOT_FOUND",
            AppError::InvalidIntegratorSignature => "INVALID_INTEGRATOR_SIGNATURE",
            AppError::IssuerRegistrationChallengeNotFound => {
                "ISSUER_REGISTRATION_CHALLENGE_NOT_FOUND"
            }
            AppError::IssuerRegistrationChallengeExpired => "ISSUER_REGISTRATION_CHALLENGE_EXPIRED",
            AppError::InvalidProofOfPossession => "INVALID_PROOF_OF_POSSESSION",
            AppError::DeclaredNetworkMismatch => "DECLARED_NETWORK_MISMATCH",
            AppError::IssuerNotRegisteredOnNetwork => "ISSUER_NOT_REGISTERED_ON_NETWORK",
            AppError::IssuerKeyNotRoot => "ISSUER_KEY_NOT_ROOT",
            AppError::InvalidIssuerKeyRole => "INVALID_ISSUER_KEY_ROLE",
            AppError::InvalidIssuerKeyPurpose => "INVALID_ISSUER_KEY_PURPOSE",
            AppError::IssuerKeyForbidden => "ISSUER_KEY_FORBIDDEN",
            AppError::CapabilityNotRequested => "CAPABILITY_NOT_REQUESTED",
            AppError::BindingNotFound => "BINDING_NOT_FOUND",
            AppError::GrantNotFound => "GRANT_NOT_FOUND",
            AppError::Forbidden => "FORBIDDEN",
            AppError::InvalidAchievementKey => "INVALID_ACHIEVEMENT_KEY",
            AppError::AchievementKeyTaken => "ACHIEVEMENT_KEY_TAKEN",
            AppError::AchievementDefinitionNotFound => "ACHIEVEMENT_DEFINITION_NOT_FOUND",
            AppError::ClaimVocabularyMismatch => "CLAIM_VOCABULARY_MISMATCH",
            AppError::InvalidAchievementIcon => "INVALID_ACHIEVEMENT_ICON",
            AppError::InvalidAchievementIconUrl => "INVALID_ACHIEVEMENT_ICON_URL",
            AppError::AttestationDefinitionRetired => "ATTESTATION_DEFINITION_RETIRED",
            AppError::InvalidAttestationSignature => "INVALID_ATTESTATION_SIGNATURE",
            AppError::InvalidBulkAttestationRequest => "INVALID_BULK_ATTESTATION_REQUEST",
            AppError::AttestationWriteQuotaExceeded => "ATTESTATION_WRITE_QUOTA_EXCEEDED",
            AppError::AttestationNotFound => "ATTESTATION_NOT_FOUND",
            AppError::AttestationRevocationForbidden => "ATTESTATION_REVOCATION_FORBIDDEN",
            AppError::AttestationAlreadyRevoked => "ATTESTATION_ALREADY_REVOKED",
            AppError::InvalidDiscoverQuery => "INVALID_DISCOVER_QUERY",
            AppError::TooManyFavoriteGames => "TOO_MANY_FAVORITE_GAMES",
            AppError::DuplicateFavoriteGame => "DUPLICATE_FAVORITE_GAME",
            AppError::FavoriteGameNotBound => "FAVORITE_GAME_NOT_BOUND",
            AppError::SignedTreeHeadNotFound | AppError::SignedTreeHeadNotFoundMirror { .. } => {
                "SIGNED_TREE_HEAD_NOT_FOUND"
            }
            AppError::InvalidProofQuery => "INVALID_PROOF_QUERY",
            AppError::LedgerRangeNotCommitted => "LEDGER_RANGE_NOT_COMMITTED",
            AppError::ProofVerificationFailed => "PROOF_VERIFICATION_FAILED",
            AppError::InvalidEntriesQuery => "INVALID_ENTRIES_QUERY",
            AppError::AchievementDefinitionForbidden => "ACHIEVEMENT_DEFINITION_FORBIDDEN",
            AppError::InvalidGuardianSet => "INVALID_GUARDIAN_SET",
            AppError::RecoveryNotConfigured => "RECOVERY_NOT_CONFIGURED",
            AppError::RecoveryNotAvailable => "RECOVERY_NOT_AVAILABLE",
            AppError::RecoveryRequestNotFound => "RECOVERY_REQUEST_NOT_FOUND",
            AppError::RecoveryAlreadyInProgress => "RECOVERY_ALREADY_IN_PROGRESS",
            AppError::RecoveryRateLimited => "RECOVERY_RATE_LIMITED",
            AppError::PrincipalRateLimited { .. } => "RATE_LIMITED",
            AppError::RecoveryNotReadyToFinalize => "RECOVERY_NOT_READY_TO_FINALIZE",
            AppError::RecoveryAlreadyResolved => "RECOVERY_ALREADY_RESOLVED",
            AppError::NotAGuardian => "NOT_A_GUARDIAN",
            AppError::AlreadyApproved => "ALREADY_APPROVED",
            AppError::GuildNotRecruiting => "GUILD_NOT_RECRUITING",
            AppError::InvalidJoinRequestMessage => "INVALID_JOIN_REQUEST_MESSAGE",
            AppError::GuildJoinRequestNotFound => "GUILD_JOIN_REQUEST_NOT_FOUND",
            AppError::InvalidIntegratorSchema => "INVALID_INTEGRATOR_SCHEMA",
            AppError::IntegratorSchemaNotFound => "INTEGRATOR_SCHEMA_NOT_FOUND",
            AppError::IntegratorSchemaForbidden => "INTEGRATOR_SCHEMA_FORBIDDEN",
            AppError::InvalidProtoSchema { .. } => "INVALID_PROTO_SCHEMA",
            AppError::InstanceSchemaMismatch { .. } => "INSTANCE_SCHEMA_MISMATCH",
            AppError::IntegratorDataInstanceNotFound => "INTEGRATOR_DATA_INSTANCE_NOT_FOUND",
            AppError::IntegratorDataSchemaOwnershipMismatch => {
                "INTEGRATOR_DATA_SCHEMA_OWNERSHIP_MISMATCH"
            }
            AppError::IntegratorSchemaMappingSchemaNotFound => {
                "INTEGRATOR_SCHEMA_MAPPING_SCHEMA_NOT_FOUND"
            }
            AppError::IntegratorSchemaMappingSchemaOwnershipMismatch => {
                "INTEGRATOR_SCHEMA_MAPPING_SCHEMA_OWNERSHIP_MISMATCH"
            }
            AppError::InvalidIntegratorSchemaMapping => "INVALID_INTEGRATOR_SCHEMA_MAPPING",
            AppError::IntegratorSchemaMappingNotFound => "INTEGRATOR_SCHEMA_MAPPING_NOT_FOUND",
            AppError::IntegratorSchemaMappingForbidden => "INTEGRATOR_SCHEMA_MAPPING_FORBIDDEN",
            AppError::InvalidConversationParticipants => "INVALID_CONVERSATION_PARTICIPANTS",
            AppError::NotConversationParticipant => "NOT_CONVERSATION_PARTICIPANT",
            AppError::IntegratorRecognitionForbidden => "INTEGRATOR_RECOGNITION_FORBIDDEN",
            AppError::InvalidRecognitionScope => "INVALID_RECOGNITION_SCOPE",
            AppError::DevicePairingNotFound => "DEVICE_PAIRING_NOT_FOUND",
            AppError::DevicePairingCodeGenerationFailed => "DEVICE_PAIRING_CODE_GENERATION_FAILED",
            AppError::CrossNodeLoginRequestNotFound => "CROSS_NODE_LOGIN_REQUEST_NOT_FOUND",
            AppError::CrossNodeLoginRequestCodeGenerationFailed => {
                "CROSS_NODE_LOGIN_REQUEST_CODE_GENERATION_FAILED"
            }
            AppError::PeerNetworkMismatch => "PEER_NETWORK_MISMATCH",
            AppError::InvalidLogFilter => "INVALID_LOG_FILTER",
            AppError::LogReloadFailed => "LOG_RELOAD_FAILED",
            AppError::RemoteRoleUnreachable { .. } => "REMOTE_ROLE_UNREACHABLE",
            AppError::ShardBelowMinimumReplication => "SHARD_BELOW_MINIMUM_REPLICATION",
            AppError::Database(..) => "DATABASE",
            AppError::Ledger(..) => "LEDGER",
            AppError::Index(..) => "INDEX",
        }
    }
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
            // An integrator authenticated fine and even holds `presence.publish`
            // under an active binding — this is a distinct, narrower
            // rejection than `Forbidden` below: the one thing a capability
            // grant can never authorize is claiming to be a *different*
            // integrator. Still a 403: the request is well-formed, the caller
            // just isn't allowed to make this particular claim.
            AppError::PresenceActiveInMismatch => StatusCode::FORBIDDEN,
            AppError::AlreadyFriends | AppError::FriendRequestExists => StatusCode::CONFLICT,
            AppError::NotFriends => StatusCode::NOT_FOUND,
            AppError::HandleNotFound => StatusCode::NOT_FOUND,
            AppError::DisplayNameTaken => StatusCode::CONFLICT,
            AppError::InvalidAvatarUrl
            | AppError::InvalidBio
            | AppError::InvalidPronouns
            | AppError::InvalidGenre
            | AppError::TooManyFavoriteGenres
            | AppError::InvalidStatus
            | AppError::TooManyLinks
            | AppError::InvalidLink
            | AppError::InvalidTimezone
            | AppError::InvalidThemeColor
            | AppError::InvalidLocation
            | AppError::InvalidMainGuild => StatusCode::BAD_REQUEST,
            AppError::SelfBlock => StatusCode::BAD_REQUEST,
            AppError::AlreadyBlocked => StatusCode::CONFLICT,
            AppError::BlockNotFound => StatusCode::NOT_FOUND,
            AppError::SigningKeyNotFound
            | AppError::DeviceGrantNotFound
            | AppError::PasskeyNotFound
            | AppError::RollbackEventNotEligible => StatusCode::NOT_FOUND,
            AppError::InvalidRollbackWindow => StatusCode::BAD_REQUEST,
            AppError::RollbackNoCompletedRecovery
            | AppError::RollbackNotReversible(_)
            | AppError::RollbackAlreadyReversed => StatusCode::CONFLICT,
            AppError::DeviceGrantExpired => StatusCode::GONE,
            AppError::ApproverKeyInvalid | AppError::InvalidGrantSignature => {
                StatusCode::UNAUTHORIZED
            }
            // Issue #698: "you have no key to sign with at all" is an
            // actionable account-state problem (go register one) — 409,
            // not 401/403.
            AppError::NoRegisteredSigningKey => StatusCode::CONFLICT,
            // The caller has a key but didn't sign this specific request,
            // or the signature itself doesn't verify — both are auth
            // failures on this one request, same status as
            // `InvalidGrantSignature` above.
            AppError::FreshSignatureRequired | AppError::InvalidFreshSignature => {
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
            | AppError::InvalidVisibility
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
            // membership-gated read/write or a row-not-affected
            // check on a leave/remove/role-change mutation.
            AppError::NotGuildMember => StatusCode::FORBIDDEN,
            AppError::ChannelNotFound | AppError::MessageNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidChannelName
            | AppError::InvalidChannelTopic
            | AppError::MessageTooLong => StatusCode::BAD_REQUEST,
            AppError::ChannelArchived => StatusCode::CONFLICT,
            AppError::GuildEventNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidEventTitle
            | AppError::InvalidEventDescription
            | AppError::InvalidEventTimeRange
            | AppError::InvalidRsvpStatus
            | AppError::InvalidResourceKind
            | AppError::InvalidPermission => StatusCode::BAD_REQUEST,
            AppError::PermissionOverrideNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidIntegratorSlug
            | AppError::InvalidIntegratorKey
            | AppError::InvalidIntegratorCategory => StatusCode::BAD_REQUEST,
            AppError::IntegratorSlugTaken => StatusCode::CONFLICT,
            AppError::IntegratorNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidIntegratorsListQuery => StatusCode::BAD_REQUEST,
            AppError::InvalidAchievementsListQuery => StatusCode::BAD_REQUEST,
            // Auth-failure reasons for the integrator challenge-response scheme
            // all collapse to 401, same as `WebauthnFailed`/
            // `InvalidEventSignature` above — the specific reason is useful
            // for a legitimate caller debugging its own integration, not
            // something worth a different status code for.
            AppError::IntegratorChallengeNotFound
            | AppError::IntegratorChallengeExpired
            | AppError::IntegratorKeyNotFound
            | AppError::InvalidIntegratorSignature
            | AppError::IssuerKeyNotRoot => StatusCode::UNAUTHORIZED,
            // Same "collapse to 401" posture as the integrator challenge-
            // response scheme above — a registration-challenge failure
            // reason is useful for a legitimate caller debugging its own
            // integration, not worth a different status code.
            AppError::IssuerRegistrationChallengeNotFound
            | AppError::IssuerRegistrationChallengeExpired
            | AppError::InvalidProofOfPossession => StatusCode::UNAUTHORIZED,
            AppError::DeclaredNetworkMismatch => StatusCode::BAD_REQUEST,
            // Signature-authentic but not admitted on this network —
            // a real, distinct 403, not folded into the 401s above: the
            // caller proved key possession just fine, this network simply
            // hasn't admitted the key.
            AppError::IssuerNotRegisteredOnNetwork => StatusCode::FORBIDDEN,
            AppError::InvalidIssuerKeyRole => StatusCode::BAD_REQUEST,
            AppError::InvalidIssuerKeyPurpose => StatusCode::BAD_REQUEST,
            AppError::IssuerKeyForbidden => StatusCode::FORBIDDEN,
            AppError::CapabilityNotRequested => StatusCode::BAD_REQUEST,
            AppError::BindingNotFound | AppError::GrantNotFound => StatusCode::NOT_FOUND,
            // Authenticated (as *some* integrator), but that
            // integrator lacks the specific capability it needs for this
            // request. Generic 403 body, same as every other authorization
            // (as opposed to authentication) failure in this file — see
            // `require_capability`'s own doc comment for why the body
            // never says *which* of "no binding" / "wrong integrator" / "no
            // grant" / "revoked grant" applied.
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::InvalidAchievementKey => StatusCode::BAD_REQUEST,
            AppError::AchievementKeyTaken => StatusCode::CONFLICT,
            AppError::AchievementDefinitionNotFound => StatusCode::NOT_FOUND,
            // A route mismatch (an Integrator hitting the milestones route, or
            // vice versa) is caught by checking the issuer's own real
            // registered category, same "authenticated fine as *some*
            // issuer, but not authorized for this specific action" shape
            // as `AchievementDefinitionForbidden` just above — 403, not a
            // generic 400/401.
            AppError::ClaimVocabularyMismatch => StatusCode::FORBIDDEN,
            AppError::InvalidAchievementIcon | AppError::InvalidAchievementIconUrl => {
                StatusCode::BAD_REQUEST
            }
            AppError::AttestationDefinitionRetired => StatusCode::CONFLICT,
            AppError::InvalidAttestationSignature => StatusCode::UNAUTHORIZED,
            AppError::InvalidBulkAttestationRequest => StatusCode::BAD_REQUEST,
            AppError::AttestationWriteQuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
            AppError::AttestationNotFound => StatusCode::NOT_FOUND,
            AppError::AttestationRevocationForbidden => StatusCode::FORBIDDEN,
            AppError::AttestationAlreadyRevoked => StatusCode::CONFLICT,
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
            AppError::RecoveryRateLimited | AppError::PrincipalRateLimited { .. } => {
                StatusCode::TOO_MANY_REQUESTS
            }
            AppError::RecoveryNotReadyToFinalize => StatusCode::CONFLICT,
            AppError::RecoveryAlreadyResolved => StatusCode::CONFLICT,
            AppError::NotAGuardian => StatusCode::FORBIDDEN,
            AppError::AlreadyApproved => StatusCode::CONFLICT,
            AppError::GuildNotRecruiting => StatusCode::FORBIDDEN,
            AppError::InvalidJoinRequestMessage => StatusCode::BAD_REQUEST,
            AppError::GuildJoinRequestNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidDiscoverQuery => StatusCode::BAD_REQUEST,
            AppError::InvalidIntegratorSchema => StatusCode::BAD_REQUEST,
            AppError::IntegratorSchemaNotFound => StatusCode::NOT_FOUND,
            // Same shape as `AchievementDefinitionForbidden`: authenticated
            // fine as *some* integrator, but that integrator isn't the one named by
            // the `{slug}` path segment — never allowed to publish a
            // schema attributed to another integrator's id.
            AppError::IntegratorSchemaForbidden => StatusCode::FORBIDDEN,
            AppError::InvalidProtoSchema { .. } => StatusCode::BAD_REQUEST,
            AppError::InstanceSchemaMismatch { .. } => StatusCode::BAD_REQUEST,
            AppError::IntegratorDataInstanceNotFound => StatusCode::NOT_FOUND,
            // Same "authenticated fine as *some* integrator, but not allowed to
            // make this specific claim" shape as `IntegratorSchemaForbidden`.
            AppError::IntegratorDataSchemaOwnershipMismatch => StatusCode::FORBIDDEN,
            // A mapping's `from`/`to` referencing a schema id that doesn't
            // exist at all — a well-formed request, missing referent.
            AppError::IntegratorSchemaMappingSchemaNotFound => StatusCode::NOT_FOUND,
            // Same shape as `IntegratorDataSchemaOwnershipMismatch`: the
            // referenced schema exists, just isn't owned by the caller.
            AppError::IntegratorSchemaMappingSchemaOwnershipMismatch => StatusCode::FORBIDDEN,
            AppError::InvalidIntegratorSchemaMapping => StatusCode::BAD_REQUEST,
            AppError::IntegratorSchemaMappingNotFound => StatusCode::NOT_FOUND,
            // Same shape as `IntegratorSchemaForbidden`: authenticated fine as
            // *some* integrator, but that integrator isn't the one named by
            // the `{slug}` path segment.
            AppError::IntegratorSchemaMappingForbidden => StatusCode::FORBIDDEN,
            AppError::InvalidConversationParticipants => StatusCode::BAD_REQUEST,
            // Same status as `NotGuildMember`: an authorization fact, not a
            // missing resource, and — per this variant's own doc comment —
            // deliberately reused for the blocked-pair-on-send case too, so
            // the response never distinguishes the two.
            AppError::NotConversationParticipant => StatusCode::FORBIDDEN,
            AppError::IntegratorRecognitionForbidden => StatusCode::FORBIDDEN,
            AppError::InvalidRecognitionScope => StatusCode::BAD_REQUEST,
            AppError::DevicePairingNotFound => StatusCode::NOT_FOUND,
            AppError::DevicePairingCodeGenerationFailed => StatusCode::CONFLICT,
            AppError::CrossNodeLoginRequestNotFound => StatusCode::NOT_FOUND,
            AppError::CrossNodeLoginRequestCodeGenerationFailed => StatusCode::CONFLICT,
            // Mirrors the genesis-mismatch-is-fatal precedent: this
            // node's own network_id is never negotiable against a peer's
            // claim, but it's a per-request rejection, not fatal to the
            // node itself the way a genesis mismatch at startup is.
            AppError::PeerNetworkMismatch => StatusCode::CONFLICT,
            AppError::TooManyFavoriteGames | AppError::DuplicateFavoriteGame => {
                StatusCode::BAD_REQUEST
            }
            // The affinity this would-be pin claims doesn't currently exist
            // (zero actively-bound members) — a well-formed request the
            // caller isn't allowed to make, same "authenticated fine, just
            // not allowed to claim this" shape as `PresenceActiveInMismatch`
            // above, not a 404 (the integrator itself may well exist).
            AppError::FavoriteGameNotBound => StatusCode::FORBIDDEN,
            AppError::SignedTreeHeadNotFound | AppError::SignedTreeHeadNotFoundMirror { .. } => {
                StatusCode::NOT_FOUND
            }
            AppError::InvalidProofQuery => StatusCode::BAD_REQUEST,
            // "Doesn't exist *yet*," not "never will" — a request for a
            // seq/tree_size beyond what's actually committed so far. 404,
            // matching every other not-found in this file, and explicitly
            // never a fabricated/empty proof.
            AppError::LedgerRangeNotCommitted => StatusCode::NOT_FOUND,
            // Should never actually happen — every proof this server
            // returns is self-verified before the response is built (see
            // `crate::settlement`). If it ever does, that's an internal bug
            // in proof generation, not a client-input problem.
            AppError::ProofVerificationFailed => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::InvalidEntriesQuery => StatusCode::BAD_REQUEST,
            AppError::InvalidLogFilter => StatusCode::BAD_REQUEST,
            AppError::LogReloadFailed => StatusCode::INTERNAL_SERVER_ERROR,
            // 503, not 409/403: the request itself is fine and the
            // identity_id/display_name are both still available — this is
            // a temporary, shard-wide condition that resolves on its own
            // once enough peers confirm mirroring (or, if not, is a signal
            // for the operator, not the caller, to act on).
            AppError::ShardBelowMinimumReplication => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Database(_) | AppError::Ledger(_) | AppError::Index(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            // Issue #661: a real dependency-down condition, not the
            // client's fault and not "the data doesn't exist" — 503, the
            // conventional status for "this server is fine but something
            // it depends on isn't," distinct from the 500s above.
            AppError::RemoteRoleUnreachable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        };
        // Never leak internal error detail (e.g. SQL error text) to the client —
        // log it server-side once real observability exists; for now the
        // generic message is the safe default.
        let message = match &self {
            AppError::Database(_) | AppError::Ledger(_) | AppError::Index(_) => {
                "internal server error".to_string()
            }
            AppError::ProofVerificationFailed => "internal server error".to_string(),
            // Issue #661: `detail` is diagnostic text about this node's
            // own internal deployment (which internal URL failed, the raw
            // network error) — never worth handing to an external caller,
            // same posture as the storage-error variants above. `role`
            // alone (not internal-topology detail) is genuinely useful to
            // a caller/SDK deciding whether to retry, so it stays in the
            // message.
            AppError::RemoteRoleUnreachable { role, .. } => {
                format!("remote role '{role}' is unreachable")
            }
            other => other.to_string(),
        };
        let mut body = json!({ "error": message, "code": self.code() });
        if let AppError::SignedTreeHeadNotFoundMirror { peers } = &self {
            body["is_mirror"] = json!(true);
            body["mirror_peers"] = json!(peers);
        }
        let mut response = (status, Json(body)).into_response();
        if let AppError::PrincipalRateLimited { retry_after_secs } = &self {
            if let Ok(value) = axum::http::HeaderValue::from_str(&retry_after_secs.to_string()) {
                response.headers_mut().insert("retry-after", value);
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #661: an `IndexError::RemoteUnreachable` (raised by
    /// `crate::internal_role::RemoteIndexer` on a network failure or
    /// non-success response) must map to `AppError::RemoteRoleUnreachable`
    /// — a distinct 503, not the generic 500 `AppError::Index` carries —
    /// never silently fall through the `#[from]`-style catch-all a plain
    /// derive would have given every other `IndexError` variant.
    #[test]
    fn remote_unreachable_index_error_maps_to_remote_role_unreachable() {
        let err: AppError =
            avalon_indexer::IndexError::RemoteUnreachable("connection refused".to_string()).into();
        assert!(matches!(
            err,
            AppError::RemoteRoleUnreachable { ref role, .. } if role == "indexer"
        ));
        assert_eq!(err.code(), "REMOTE_ROLE_UNREACHABLE");
    }

    /// Every other `IndexError` variant still takes the ordinary
    /// `AppError::Index` path, so this mapping stays additive rather than
    /// reclassifying unrelated index failures.
    #[test]
    fn other_index_errors_still_map_to_index() {
        let err: AppError = avalon_indexer::IndexError::DisplayNameTaken.into();
        assert!(matches!(err, AppError::Index(_)));
        assert_eq!(err.code(), "INDEX");
    }
}
