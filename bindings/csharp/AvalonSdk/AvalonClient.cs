using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text.Json.Serialization;
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
        /// Sent as the <c>x-avalon-integrator-key-id</c> header — the only
        /// spelling the server accepts since #290 — once this client is
        /// wired to a real server. Mirrors <c>avalon-sdk</c>'s
        /// <c>integrator_credential_key_id</c>.
        /// </summary>
        public string IntegratorCredentialKeyId { get; }
    }

    internal sealed class MeResponse
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }
    }

    internal sealed class MyGrantsResponse
    {
        public List<string> Capabilities { get; set; } = new List<string>();
    }

    /// <summary>
    /// Entry point for an integrator (game, app, or service) integrating
    /// Avalon Protocol. Mirrors the Rust reference SDK (<c>avalon-sdk</c>)
    /// — an integrator creates a client, authenticates an identity, then
    /// works only through the returned <see cref="Session"/>, which
    /// enforces whichever capabilities were actually granted.
    /// </summary>
    public sealed class AvalonClient
    {
        private readonly AvalonConfig _config;
        private readonly HttpClient _http;

        public AvalonClient(AvalonConfig config)
        {
            _config = config;
            _http = new HttpClient();
        }

        /// <summary>
        /// Exchanges an identity's existing Avalon session token (obtained via the Hub or a
        /// direct login, not by this SDK — an integrator never creates identities itself) for
        /// a Session scoped to this integrator. GET /me for the identity, GET /me/grants
        /// (issue #27) for this integrator's own active capability grants — a non-success
        /// grants response is treated as "no grants" rather than an authentication failure.
        /// </summary>
        public async Task<Session> AuthenticateAsync(string identityToken, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{_config.ServerUrl}/me");
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", identityToken);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw new AvalonAuthenticationFailedException();
            }
            var me = await Session.ReadJsonAsync<MeResponse>(response, ct).ConfigureAwait(false);

            var granted = await FetchGrantedAsync(identityToken, ct).ConfigureAwait(false);

            return new Session(me.IdentityId, granted, _http, _config.ServerUrl, identityToken);
        }

        private async Task<IReadOnlyList<string>> FetchGrantedAsync(string identityToken, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{_config.ServerUrl}/me/grants");
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", identityToken);
            request.Headers.Add("x-avalon-integrator-key-id", _config.IntegratorCredentialKeyId);

            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                return Array.Empty<string>();
            }
            var body = await Session.ReadJsonAsync<MyGrantsResponse>(response, ct).ConfigureAwait(false);
            return body.Capabilities;
        }
    }
}
