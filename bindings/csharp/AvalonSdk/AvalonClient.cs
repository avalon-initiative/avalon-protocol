using System;
using System.Collections.Generic;
using System.Linq;
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
        public AvalonConfig(
            string serverUrl,
            string integratorCredentialKeyId,
            string? integratorSlug = null,
            byte[]? signingKey = null)
        {
            ServerUrl = serverUrl;
            IntegratorCredentialKeyId = integratorCredentialKeyId;
            IntegratorSlug = integratorSlug;
            SigningKey = signingKey;
        }

        public string ServerUrl { get; }

        /// <summary>
        /// Sent as the <c>x-avalon-integrator-key-id</c> header — the only
        /// spelling the server accepts since #290 — once this client is
        /// wired to a real server. Mirrors <c>avalon-sdk</c>'s
        /// <c>integrator_credential_key_id</c>.
        /// </summary>
        public string IntegratorCredentialKeyId { get; }

        /// <summary>
        /// This integrator's own registered slug — required only by
        /// <see cref="Session.IssueAchievementAsync"/>. <c>null</c> for a
        /// read-only integration. Mirrors <c>AvalonConfig::integrator_slug</c>.
        /// </summary>
        public string? IntegratorSlug { get; }

        /// <summary>
        /// This integrator's own 32-byte Ed25519 signing key seed, held only
        /// in this process — the server never sees it, only a detached
        /// signature. <c>null</c> for a read-only integration; required by
        /// <see cref="Session.IssueAchievementAsync"/>. Mirrors
        /// <c>AvalonConfig::signing_key</c>.
        /// </summary>
        public byte[]? SigningKey { get; }
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
    public sealed partial class AvalonClient
    {
        private readonly AvalonConfig _config;
        private readonly HttpClient _http;

        /// <summary>See <see cref="AvalonConfig.ServerUrl"/> — exposed internally so
        /// <see cref="CrossNodeLogin"/> (a separate class, not a partial member of this one) can
        /// address the node this client talks to without this SDK's HTTP details becoming public
        /// API. Mirrors the Rust SDK's <c>AvalonClient::config.server_url</c> access from
        /// <c>cross_node_login.rs</c>.</summary>
        internal string ServerUrl => _config.ServerUrl;

        /// <summary>See the constructor's own doc comment on why this is injected. Exposed
        /// internally for the same reason <see cref="ServerUrl"/> is.</summary>
        internal HttpClient Http => _http;

        /// <summary>
        /// <paramref name="httpClient"/> is injected rather than always
        /// constructed internally, so a Unity project (or a test) can supply
        /// its own <see cref="HttpMessageHandler"/> — a fresh
        /// <see cref="HttpClient"/> is used when none is given.
        /// </summary>
        public AvalonClient(AvalonConfig config, HttpClient? httpClient = null)
        {
            _config = config;
            _http = httpClient ?? new HttpClient();
        }

        /// <summary>
        /// Exchanges an identity's existing Avalon session token (obtained via the Hub or a
        /// direct login, not by this SDK — an integrator never creates identities itself) for
        /// a Session scoped to this integrator. GET /me for the identity/profile, GET /me/grants
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
                throw new AuthenticationFailedException();
            }
            var me = await Session.ReadJsonAsync<Avalon.Sdk.Generated.ProfileResponse>(response, ct).ConfigureAwait(false);

            var granted = await FetchGrantedAsync(identityToken, ct).ConfigureAwait(false);

            var identity = new Identity(me.IdentityId, me.IdentityCreatedAt);
            var profile = new Profile(me.IdentityId)
            {
                DisplayName = me.DisplayName,
                AvatarUrl = me.AvatarUrl,
                Bio = me.Bio,
                FavoriteGenres = me.FavoriteGenres.Select(g => (Genre)Enum.Parse(typeof(Genre), g.ToString())).ToList(),
                Pronouns = me.Pronouns,
                BannerUrl = me.BannerUrl,
                Status = me.Status,
                Links = new List<string>(me.Links),
                Timezone = me.Timezone,
                ThemeColor = me.ThemeColor,
                Location = me.Location,
                MainGuild = me.MainGuild,
            };

            return new Session(
                identity,
                profile,
                granted,
                _http,
                _config.ServerUrl,
                identityToken,
                _config.IntegratorCredentialKeyId,
                _config.IntegratorSlug,
                _config.SigningKey);
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
