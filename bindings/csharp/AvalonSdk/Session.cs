using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>Mirrors <c>SdkError::Unauthorized</c> — a non-success GET /me response, i.e.
    /// the identity token itself was rejected.</summary>
    public sealed class AuthenticationFailedException : Exception
    {
        public AuthenticationFailedException() : base("authentication failed")
        {
        }
    }

    /// <summary>Mirrors <c>SdkError::CapabilityNotGranted</c> — the session token is fine but
    /// this integrator hasn't been granted the capability a method requires. Thrown client-side
    /// as a fast-fail by <see cref="Session"/>'s own <c>Require</c> check before any request is
    /// made; the server enforces the same thing independently (issue #28) — this is not the
    /// security boundary.</summary>
    public sealed class CapabilityNotGrantedException : Exception
    {
        public CapabilityNotGrantedException(string capability)
            : base("capability not granted: " + capability)
        {
        }
    }

    /// <summary>
    /// A non-success response from avalon-server that isn't one of the more specific exceptions
    /// above — transport/HTTP failure, or a status this SDK doesn't yet map more precisely.
    /// Mirrors <c>SdkError</c>'s transport-level variants (<c>Unavailable</c>/<c>Protocol</c>).
    /// </summary>
    public sealed class AvalonRequestException : Exception
    {
        public AvalonRequestException(System.Net.HttpStatusCode statusCode)
            : base("avalon-server returned " + statusCode)
        {
            StatusCode = statusCode;
        }

        public System.Net.HttpStatusCode StatusCode { get; }
    }

    /// <summary>
    /// A presence websocket connection failed. Mirrors <c>SdkError::WebSocket</c>.
    /// </summary>
    public sealed class AvalonWebSocketException : Exception
    {
        public AvalonWebSocketException(string message) : base(message)
        {
        }
    }

    /// <summary>
    /// The server rejected a conversation read/send with "not a participant" — deliberately
    /// carries nothing beyond that (issue #97: never reveal a block, not even indirectly).
    /// Mirrors <c>SdkError::NotConversationParticipant</c>.
    /// </summary>
    public sealed class NotConversationParticipantException : Exception
    {
        public NotConversationParticipantException()
            : base("not a participant in this conversation")
        {
        }
    }

    /// <summary>
    /// <see cref="Session.IssueAchievementAsync"/> needs this integrator's own slug and signing
    /// key (<see cref="AvalonConfig.IntegratorSlug"/>/<see cref="AvalonConfig.SigningKey"/>) to
    /// authenticate the issuing request and sign the attestation locally — neither is required
    /// for a read-only integration, so this is thrown, without ever making an HTTP call, when a
    /// caller reaches for issuance without having supplied them. Mirrors
    /// <c>SdkError::MissingIssuerCredentials</c>.
    /// </summary>
    public sealed class MissingIssuerCredentialsException : Exception
    {
        public MissingIssuerCredentialsException()
            : base("this integrator's IntegratorSlug/SigningKey were not configured")
        {
        }
    }

    /// <summary>A fixed, small controlled vocabulary for <see cref="Profile.FavoriteGenres"/> —
    /// mirrors <c>avalon_protocol::identity::Genre</c>.</summary>
    [JsonConverter(typeof(JsonStringEnumConverter))]
    public enum Genre
    {
        Action,
        Adventure,
        Rpg,
        Strategy,
        Simulation,
        Puzzle,
        Racing,
        Sports,
        Horror,
        Sandbox,
        Mmo,
        Shooter,
        Platformer,
        Party,
    }

    /// <summary>The persistent, network-level user identity — never references any
    /// integrator's own character schema. Mirrors <c>avalon_protocol::identity::Identity</c>.
    /// Populated once by <see cref="AvalonClient.AuthenticateAsync"/>; no network call of its
    /// own.</summary>
    public sealed class Identity
    {
        public Identity(Guid id, DateTimeOffset createdAt)
        {
            Id = id;
            CreatedAt = createdAt;
        }

        public Guid Id { get; }
        public DateTimeOffset CreatedAt { get; }
    }

    /// <summary>User-controlled, human-facing profile data — deliberately small, deliberately
    /// not where integrator-specific data lives (see <c>docs/stakeholders/Proposal.md</c> §19).
    /// Mirrors <c>avalon_protocol::identity::Profile</c>. As it was when <c>AuthenticateAsync</c>
    /// ran — not re-fetched automatically after a subsequent profile edit made through another
    /// client (e.g. the Hub).</summary>
    public sealed class Profile
    {
        public Profile(Guid identityId)
        {
            IdentityId = identityId;
        }

        public Guid IdentityId { get; }
        public string DisplayName { get; set; } = "";
        public string? AvatarUrl { get; set; }
        public string? Bio { get; set; }
        public List<Genre> FavoriteGenres { get; set; } = new List<Genre>();
        public string? Pronouns { get; set; }
        public string? BannerUrl { get; set; }
        public string? Status { get; set; }
        public List<string> Links { get; set; } = new List<string>();
        public string? Timezone { get; set; }
        public string? ThemeColor { get; set; }
        public string? Location { get; set; }
        public Guid? MainGuild { get; set; }
    }

    /// <summary>
    /// An authenticated identity session scoped to whichever capabilities were
    /// actually granted. Every read/write method checks its own required
    /// capability rather than trusting the caller — see docs/stakeholders/Proposal.md §13.
    /// Split across Session.cs (this file), Social.cs, Guilds.cs, Conversations.cs,
    /// Achievements.cs — one partial-class file per matching crates/sdk/src/*.rs module.
    /// </summary>
    public sealed partial class Session
    {
        private readonly HashSet<string> _grantedCapabilities;

        /// <summary>Backs the public string <see cref="IdentityId"/> — kept as a real
        /// <see cref="Guid"/> internally so Social.cs/Guilds.cs can compare it against
        /// other identity ids without reparsing a string on every call.</summary>
        internal readonly Guid IdentityGuid;

        internal readonly HttpClient Http;
        internal readonly string ServerUrl;
        internal readonly string Token;

        /// <summary>This integrator's own registered key id
        /// (<see cref="AvalonConfig.IntegratorCredentialKeyId"/>) — the same value already
        /// used for GET /me/grants, reused by <see cref="Achievements.IssueAchievementAsync"/>-
        /// adjacent code as the challenge-response and embedded-proof key id.</summary>
        internal readonly string IntegratorKeyId;

        /// <summary>See <see cref="AvalonConfig.IntegratorSlug"/>.</summary>
        internal readonly string? IntegratorSlug;

        /// <summary>See <see cref="AvalonConfig.SigningKey"/>.</summary>
        internal readonly byte[]? SigningKey;

        internal Session(
            Identity identity,
            Profile profile,
            IEnumerable<string> grantedCapabilities,
            HttpClient http,
            string serverUrl,
            string token,
            string integratorKeyId,
            string? integratorSlug,
            byte[]? signingKey)
        {
            IdentityValue = identity;
            ProfileValue = profile;
            IdentityGuid = identity.Id;
            _grantedCapabilities = new HashSet<string>(grantedCapabilities);
            Http = http;
            ServerUrl = serverUrl;
            Token = token;
            IntegratorKeyId = integratorKeyId;
            IntegratorSlug = integratorSlug;
            SigningKey = signingKey;
        }

        private Identity IdentityValue { get; }
        private Profile ProfileValue { get; }

        /// <summary>This session's own identity id, as a string — no network call, populated
        /// once by <c>AuthenticateAsync</c>.</summary>
        public string IdentityId => IdentityValue.Id.ToString();

        /// <summary>This session's own identity (id and creation time). Mirrors the Rust
        /// SDK's <c>Session::identity()</c>.</summary>
        public Identity Identity => IdentityValue;

        /// <summary>This session's own profile, as it was when <c>AuthenticateAsync</c> ran.
        /// Mirrors the Rust SDK's <c>Session::profile()</c>.</summary>
        public Profile Profile => ProfileValue;

        /// <summary>
        /// Test-only construction that never touches the network — mirrors the Rust SDK's
        /// <c>test_session</c> helper (gated behind its <c>test-util</c> feature there).
        /// The default server URL is deliberately unroutable, same reasoning as the Rust
        /// helper: a test that forgets to stub its handler should fail loudly, not hang.
        /// </summary>
        internal static Session ForTesting(
            IEnumerable<string> grantedCapabilities,
            HttpClient? http = null,
            string serverUrl = "http://127.0.0.1:1",
            string token = "test-token",
            Guid? identityId = null,
            string integratorKeyId = "test-integrator-key",
            string? integratorSlug = null,
            byte[]? signingKey = null,
            Profile? profile = null)
        {
            var id = identityId ?? Guid.NewGuid();
            return new Session(
                new Identity(id, DateTimeOffset.UtcNow),
                profile ?? new Profile(id) { DisplayName = "test" },
                grantedCapabilities,
                http ?? new HttpClient(),
                serverUrl,
                token,
                integratorKeyId,
                integratorSlug,
                signingKey);
        }

        /// <summary>
        /// Every capability-gated method across Session.cs/Social.cs/Guilds.cs/Conversations.cs/
        /// Achievements.cs calls this first, same convention the Rust SDK established — internal
        /// rather than private so the GuildHandle/ChannelHandle/ConversationHandle wrapper classes
        /// (not partial-class members of Session, since they need their own identity/state) can
        /// call it too, mirroring how guilds.rs/conversations.rs call session.require(...)
        /// through a borrowed &amp;Session.
        /// </summary>
        internal void Require(string capability)
        {
            if (!_grantedCapabilities.Contains(capability))
            {
                throw new CapabilityNotGrantedException(capability);
            }
        }

        internal bool HasCapability(string capability) => _grantedCapabilities.Contains(capability);

        /// <summary>Translates a non-success HTTP response into the matching exception.</summary>
        internal static Exception ServerError(System.Net.HttpStatusCode status) => new AvalonRequestException(status);
    }
}
