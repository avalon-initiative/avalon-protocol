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
        public AvalonConfig(string serverUrl, string integratorCredentialKeyId)
        {
            ServerUrl = serverUrl;
            IntegratorCredentialKeyId = integratorCredentialKeyId;
        }

        public string ServerUrl { get; }

        /// <summary>
        /// Sent as the <c>x-avalon-integrator-key-id</c> header (the
        /// generic name the server now documents as canonical; it still
        /// accepts the older <c>x-avalon-game-key-id</c> too) once this
        /// client is wired to a real server — mirrors
        /// <c>avalon-sdk</c>'s <c>game_credential_key_id</c>.
        /// </summary>
        public string IntegratorCredentialKeyId { get; }
    }

    /// <summary>
    /// Entry point for an integrator (game, app, or service) integrating
    /// Avalon Protocol. Mirrors the Rust reference SDK (<c>avalon-sdk</c>)
    /// — an integrator creates a client, authenticates an identity, then
    /// works only through the returned <see cref="Session"/>, which
    /// enforces whichever capabilities were actually granted.
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

        public Task<Session> AuthenticateAsync(string userToken, CancellationToken ct = default)
        {
            throw new NotImplementedException("AvalonClient.AuthenticateAsync is not yet implemented.");
        }
    }
}
