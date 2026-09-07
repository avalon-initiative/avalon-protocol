using System;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>
    /// Configuration for connecting to an Avalon network deployment.
    /// </summary>
    public sealed class AvalonConfig
    {
        public AvalonConfig(string serverUrl, string gameCredentialKeyId)
        {
            ServerUrl = serverUrl;
            GameCredentialKeyId = gameCredentialKeyId;
        }

        public string ServerUrl { get; }
        public string GameCredentialKeyId { get; }
    }

    /// <summary>
    /// Entry point for a game integrating Avalon Protocol. Mirrors the Rust
    /// reference SDK (<c>avalon-sdk</c>) — a game creates a client,
    /// authenticates a player, then works only through the returned
    /// <see cref="Session"/>, which enforces whichever capabilities were
    /// actually granted.
    /// </summary>
    /// <remarks>
    /// Scaffold only: not yet wired to a real Avalon server.
    /// </remarks>
    public sealed class AvalonClient
    {
        private readonly AvalonConfig _config;

        public AvalonClient(AvalonConfig config)
        {
            _config = config;
        }

        public Task<Session> AuthenticateAsync(string playerToken, CancellationToken ct = default)
        {
            throw new NotImplementedException("AvalonClient.AuthenticateAsync is not yet implemented.");
        }
    }
}
