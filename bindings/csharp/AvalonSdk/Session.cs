using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>Mirrors SdkError::AuthenticationFailed — a non-success GET /me response.</summary>
    public sealed class AvalonAuthenticationFailedException : Exception
    {
        public AvalonAuthenticationFailedException() : base("authentication failed")
        {
        }
    }

    public sealed class CapabilityNotGrantedException : Exception
    {
        public CapabilityNotGrantedException(string capability)
            : base("capability not granted: " + capability)
        {
        }
    }

    /// <summary>
    /// A non-success response from avalon-server. Mirrors <c>SdkError::ServerError</c>.
    /// </summary>
    public sealed class AvalonServerException : Exception
    {
        public AvalonServerException(System.Net.HttpStatusCode statusCode)
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

    public sealed class AchievementAttestation
    {
        public AchievementAttestation(string id, string issuer, string achievement, DateTimeOffset issuedAt)
        {
            Id = id;
            Issuer = issuer;
            Achievement = achievement;
            IssuedAt = issuedAt;
        }

        public string Id { get; }
        public string Issuer { get; }
        public string Achievement { get; }
        public DateTimeOffset IssuedAt { get; }
    }

    /// <summary>
    /// An authenticated identity session scoped to whichever capabilities were
    /// actually granted. Every read/write method checks its own required
    /// capability rather than trusting the caller — see docs/stakeholders/Proposal.md §13.
    /// Split across Session.cs (this file), Social.cs, Guilds.cs, Conversations.cs
    /// — one partial-class file per matching crates/sdk/src/*.rs module.
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

        internal Session(Guid identityId, IEnumerable<string> grantedCapabilities, HttpClient http, string serverUrl, string token)
        {
            IdentityGuid = identityId;
            IdentityId = identityId.ToString();
            _grantedCapabilities = new HashSet<string>(grantedCapabilities);
            Http = http;
            ServerUrl = serverUrl;
            Token = token;
        }

        public string IdentityId { get; }

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
            Guid? identityId = null)
        {
            return new Session(
                identityId ?? Guid.NewGuid(),
                grantedCapabilities,
                http ?? new HttpClient(),
                serverUrl,
                token);
        }

        /// <summary>
        /// Every capability-gated method across Session.cs/Social.cs/Guilds.cs/Conversations.cs
        /// calls this first, same convention the Rust SDK established — internal rather than
        /// private so the GuildHandle/ChannelHandle/ConversationHandle wrapper classes (not
        /// partial-class members of Session, since they need their own identity/state) can
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
        internal static Exception ServerError(System.Net.HttpStatusCode status) => new AvalonServerException(status);

        public Task<IReadOnlyList<AchievementAttestation>> GetAchievementsAsync(CancellationToken ct = default)
        {
            Require("achievements.read");
            throw new NotImplementedException(
                "GetAchievementsAsync is out of scope for #396 — see the PR description for why.");
        }

        public Task IssueAchievementAsync(string achievement, CancellationToken ct = default)
        {
            Require("achievements.issue");
            throw new NotImplementedException(
                "IssueAchievementAsync is out of scope for #396 — see the PR description for why.");
        }
    }
}
