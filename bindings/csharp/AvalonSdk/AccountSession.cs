// AccountSession — issue #700, mirroring crates/sdk/src/account/mod.rs — a first-party
// client for an identity's own account, entirely distinct from the capability-gated
// integrator Session (Session.cs) AvalonClient.AuthenticateAsync builds. There is
// deliberately no cast/conversion operator and no AccountSession constructor/factory that
// accepts an integrator credential anywhere in its signature — #696's hard invariant that
// an integrator credential must never yield account-level power holds at the type level,
// not just by convention.
//
// Scoping decision (documented per the ticket, also in docs/architecture/sdk.md): the Rust
// AccountSession is obtained via three entry points — Register/AccountLogin (both drive a
// real WebAuthn ceremony against a virtual/software authenticator, via passkey-authenticator/
// passkey-client's "testable" feature) or ResumeAccountSession(WithSigningKey). Nothing in
// this codebase's .NET dependency set does WebAuthn ceremony work at all — LiveTests.cs
// itself seeds identities/sessions/signing keys directly via SQL rather than driving one, and
// this SDK's real audience (a Unity game binding a bearer token another surface — Hub, a
// platform's own auth — already produced) never needs to run a ceremony inside the game
// client. So this port implements only the Resume* entry points below; Register/AccountLogin
// (and AddPasskeyAsync, which also drives a registration ceremony) are intentionally not
// ported. AccountCredentials (the Rust type Register/AccountLogin hand back to log back in on
// the same device) has no C# equivalent for the same reason.

using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;
using Org.BouncyCastle.Crypto.Parameters;
using Org.BouncyCastle.Crypto.Signers;

namespace Avalon.Sdk
{
    /// <summary>
    /// A first-party session for an identity's own account (issue #700/#699) — see this
    /// file's own header comment for how one is obtained and how signing works. Distinct
    /// from <see cref="Session"/>: no shared fields, no conversion, no capability-grant
    /// model (an account's own actions are gated by what the account itself is allowed to
    /// do, not by an integrator's granted capabilities). Split across this file (the type
    /// itself, shared signing/HTTP helpers, and the AvalonClient entry points) plus
    /// AccountSession.Passkeys.cs, .Devices.cs, .Recovery.cs, .Social.cs,
    /// .Conversations.cs, .GuildAdmin.cs, .Integrations.cs — one file per matching
    /// crates/sdk/src/account/*.rs submodule.
    /// </summary>
    public sealed partial class AccountSession
    {
        private readonly HttpClient _http;
        private readonly string _serverUrl;
        private SigningKeyMaterial? _signing;

        /// <summary>This session's own bearer token — for a caller (e.g. a game backend, a
        /// test harness) that wants to persist it and later call
        /// <see cref="AvalonClient.ResumeAccountSessionAsync"/> instead of authenticating
        /// again.</summary>
        public string Token { get; }

        /// <summary>This session's own identity (id and creation time), as of whenever this
        /// session was built or last refreshed — not re-fetched automatically. Mirrors the
        /// Rust SDK's <c>AccountSession::identity()</c>.</summary>
        public Identity Identity { get; private set; }

        /// <summary>This session's own profile, as of whenever this session was built or
        /// last refreshed via <see cref="RefreshProfileAsync"/>. Mirrors the Rust SDK's
        /// <c>AccountSession::profile()</c>.</summary>
        public Profile Profile { get; private set; }

        /// <summary>The <c>identity_signing_keys.id</c> this session's local signing key
        /// resolves to server-side, if this session holds one at all — <c>null</c> for a
        /// session built via <see cref="AvalonClient.ResumeAccountSessionAsync"/> with no
        /// signing key supplied, in which case every signature-required method below sends
        /// its request unsigned (see this class's own header comment). Mirrors the Rust
        /// SDK's <c>AccountSession::signing_key_id()</c>.</summary>
        public Guid? SigningKeyId => _signing?.SigningKeyId;

        /// <summary>Backs <see cref="Identity"/>/<see cref="SigningKeyId"/> comparisons in the
        /// domain-split files without reparsing a string on every call — mirrors
        /// <see cref="Session"/>'s own <c>IdentityGuid</c> field.</summary>
        internal Guid IdentityGuid => Identity.Id;

        internal AccountSession(Identity identity, Profile profile, HttpClient http, string serverUrl, string token, SigningKeyMaterial? signing)
        {
            Identity = identity;
            Profile = profile;
            _http = http;
            _serverUrl = serverUrl;
            Token = token;
            _signing = signing;
        }

        /// <summary>This session's own locally-held Ed25519 signing key, plus the
        /// server-side <c>identity_signing_keys.id</c> it resolves to. Mirrors the Rust
        /// SDK's private <c>AccountSigningKey</c>.</summary>
        internal readonly struct SigningKeyMaterial
        {
            public SigningKeyMaterial(Ed25519PrivateKeyParameters privateKey, Guid signingKeyId)
            {
                PrivateKey = privateKey;
                SigningKeyId = signingKeyId;
            }

            public Ed25519PrivateKeyParameters PrivateKey { get; }
            public Guid SigningKeyId { get; }
        }

        /// <summary>Builds the canonical <c>avalon:&lt;action_tag&gt;:v1:&lt;field1&gt;:...</c>
        /// byte string a signature-required action signs — must match
        /// <c>signature_gate::canonical_message</c> in
        /// <c>crates/server/src/signature_gate.rs</c> (and <c>crates/sdk/src/account/mod.rs::
        /// canonical_message</c>) byte-for-byte; see this file's own unit tests.</summary>
        internal static byte[] CanonicalMessage(string actionTag, params string[] fields)
        {
            var sb = new StringBuilder("avalon:").Append(actionTag).Append(":v1");
            foreach (var field in fields)
            {
                sb.Append(':').Append(field);
            }
            return Encoding.UTF8.GetBytes(sb.ToString());
        }

        /// <summary>Signs <paramref name="actionTag"/>/<paramref name="fields"/> with this
        /// session's local key, if it has one. Every signature-required method in the
        /// sibling domain files calls this unconditionally (never a caller-supplied
        /// signature), including for the conditionally-signed endpoints (last-passkey
        /// revoke, guardian removal/threshold-raise, escalating member-role change): an
        /// unused-but-valid signature is harmless, matching the same simplification the
        /// Hub frontend and the Rust SDK already made.</summary>
        private (Guid? SigningKeyId, string? Signature) Sign(string actionTag, params string[] fields)
        {
            if (_signing is null)
            {
                return (null, null);
            }
            var message = CanonicalMessage(actionTag, fields);
            return (_signing.Value.SigningKeyId, SignRaw(message));
        }

        /// <summary>Signs <paramref name="message"/> directly with this session's local key —
        /// for the one call site (<c>ApproveDeviceGrantAsync</c>) whose signed bytes predate
        /// #698's generalized <c>avalon:&lt;tag&gt;:v1:...</c> shape enough that it's clearer
        /// to build them explicitly. Throws if this session holds no local signing key;
        /// callers must check <see cref="SigningKeyId"/> first.</summary>
        internal string SignRaw(byte[] message)
        {
            if (_signing is null)
            {
                throw new InvalidOperationException("SignRaw called without a local signing key");
            }
            var signer = new Ed25519Signer();
            signer.Init(true, _signing.Value.PrivateKey);
            signer.BlockUpdate(message, 0, message.Length);
            return Convert.ToBase64String(signer.GenerateSignature());
        }

        /// <summary>Re-fetches <c>GET /me</c> and updates <see cref="Identity"/>/
        /// <see cref="Profile"/> in place — a caller that just called
        /// <see cref="UpdateProfileAsync"/> doesn't need this (the response already
        /// reflects the new state), but one that changed the profile through another
        /// client (e.g. the Hub) does.</summary>
        public async Task RefreshProfileAsync(CancellationToken ct = default)
        {
            var (identity, profile) = await FetchMeAsync(_http, _serverUrl, Token, ct).ConfigureAwait(false);
            Identity = identity;
            Profile = profile;
        }

        /// <summary><c>PATCH /me</c> — not signature-required (#697: "profile fields are
        /// self-description, not security state"). Updates <see cref="Profile"/> in place
        /// from the response on success. Mirrors the Rust SDK's
        /// <c>AccountSession::update_profile</c>.</summary>
        public async Task UpdateProfileAsync(AccountProfileUpdate update, CancellationToken ct = default)
        {
            var body = new UpdateProfileRequest
            {
                DisplayName = update.DisplayName,
                AvatarUrl = update.AvatarUrl,
                Bio = update.Bio,
                FavoriteGenres = update.FavoriteGenres?.ConvertAll(g => g.ToString()),
                Pronouns = update.Pronouns,
                BannerUrl = update.BannerUrl,
                Status = update.Status,
                Links = update.Links,
                Timezone = update.Timezone,
                ThemeColor = update.ThemeColor,
                Location = update.Location,
            };
            var me = await PatchAsync<UpdateProfileRequest, MeResponse>("/me", body, ct).ConfigureAwait(false);
            Identity = new Identity(me.IdentityId, me.IdentityCreatedAt);
            Profile = MeResponseToProfile(me);
        }

        private static Profile MeResponseToProfile(MeResponse me) => new Profile(me.IdentityId)
        {
            DisplayName = me.DisplayName,
            AvatarUrl = me.AvatarUrl,
            Bio = me.Bio,
            FavoriteGenres = me.FavoriteGenres,
            Pronouns = me.Pronouns,
            BannerUrl = me.BannerUrl,
            Status = me.Status,
            Links = me.Links,
            Timezone = me.Timezone,
            ThemeColor = me.ThemeColor,
            Location = me.Location,
            MainGuild = me.MainGuild,
        };

        internal static async Task<(Identity Identity, Profile Profile)> FetchMeAsync(HttpClient http, string serverUrl, string token, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{serverUrl}/me");
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            using var response = await http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var me = await Session.ReadJsonAsync<MeResponse>(response, ct).ConfigureAwait(false);
            return (new Identity(me.IdentityId, me.IdentityCreatedAt), MeResponseToProfile(me));
        }

        /// <summary><c>GET /me/devices</c>, matched by base64 public key — the only way a
        /// device that only knows its own local secret key learns which server-side
        /// <c>identity_signing_keys</c> row *is* itself. Returns <c>null</c> rather than
        /// erroring when no row matches — e.g. a signing key this process holds that was
        /// never actually registered server-side.</summary>
        private static async Task<Guid?> FindOwnSigningKeyIdAsync(HttpClient http, string serverUrl, string token, string publicKeyB64, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{serverUrl}/me/devices");
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            using var response = await http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var devices = await Session.ReadJsonAsync<List<DeviceIdAndKeyResponse>>(response, ct).ConfigureAwait(false);
            foreach (var device in devices)
            {
                if (device.PublicKey == publicKeyB64)
                {
                    return device.Id;
                }
            }
            return null;
        }

        internal static async Task<SigningKeyMaterial?> ResolveSigningKeyAsync(HttpClient http, string serverUrl, string token, byte[] signingKeySeed, CancellationToken ct)
        {
            var privateKey = new Ed25519PrivateKeyParameters(signingKeySeed, 0);
            var publicKeyB64 = Convert.ToBase64String(privateKey.GeneratePublicKey().GetEncoded());
            var signingKeyId = await FindOwnSigningKeyIdAsync(http, serverUrl, token, publicKeyB64, ct).ConfigureAwait(false);
            return signingKeyId is null ? (SigningKeyMaterial?)null : new SigningKeyMaterial(privateKey, signingKeyId.Value);
        }

        private sealed class DeviceIdAndKeyResponse
        {
            public Guid Id { get; set; }

            [JsonPropertyName("public_key")]
            public string PublicKey { get; set; } = "";
        }

        private static readonly JsonSerializerOptions JsonOptions = new JsonSerializerOptions
        {
            PropertyNameCaseInsensitive = true,
        };

        private string Url(string path) => _serverUrl + path;

        internal async Task<T> GetAsync<T>(string path, CancellationToken ct) where T : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<T>(response, ct).ConfigureAwait(false);
        }

        /// <summary>Same as <see cref="GetAsync{T}"/>, but for an endpoint whose body can be
        /// a legitimate JSON <c>null</c> (an <c>Option&lt;T&gt;</c> on the Rust side, e.g.
        /// <c>GET /me/recovery/status</c>) — <see cref="Session.ReadJsonAsync{T}"/> itself
        /// throws on a null body, since for every other endpoint that indicates a real bug.</summary>
        internal async Task<T?> GetNullableAsync<T>(string path, CancellationToken ct) where T : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
#if NET5_0_OR_GREATER
            var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
#else
            var stream = await response.Content.ReadAsStreamAsync().ConfigureAwait(false);
#endif
            return await JsonSerializer.DeserializeAsync<T>(stream, JsonOptions, ct).ConfigureAwait(false);
        }

        internal async Task<T> GetQueryAsync<T>(string path, IReadOnlyList<(string Key, string Value)> query, CancellationToken ct) where T : class
        {
            var qs = string.Join("&", System.Linq.Enumerable.Select(query, kv => $"{Uri.EscapeDataString(kv.Key)}={Uri.EscapeDataString(kv.Value)}"));
            var fullPath = query.Count == 0 ? path : $"{path}?{qs}";
            return await GetAsync<T>(fullPath, ct).ConfigureAwait(false);
        }

        internal async Task<T> PostAsync<TBody, T>(string path, TBody body, CancellationToken ct) where T : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            request.Content = JsonContent(body);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<T>(response, ct).ConfigureAwait(false);
        }

        /// <summary>A <c>POST</c> with no request body at all.</summary>
        internal async Task<T> PostEmptyAsync<T>(string path, CancellationToken ct) where T : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<T>(response, ct).ConfigureAwait(false);
        }

        /// <summary>A <c>POST</c> carrying a JSON body whose handler returns an empty 200
        /// body, so the response body is discarded rather than deserialized.</summary>
        internal async Task PostNoResponseAsync<TBody>(string path, TBody body, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            request.Content = JsonContent(body);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
        }

        /// <summary>Same as <see cref="PostNoResponseAsync{TBody}"/>, but with no request
        /// body at all.</summary>
        internal async Task PostEmptyNoResponseAsync(string path, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
        }

        internal async Task<T> PatchAsync<TBody, T>(string path, TBody body, CancellationToken ct) where T : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Patch, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            request.Content = JsonContent(body);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<T>(response, ct).ConfigureAwait(false);
        }

        internal async Task<T> PutAsync<TBody, T>(string path, TBody body, CancellationToken ct) where T : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Put, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            request.Content = JsonContent(body);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<T>(response, ct).ConfigureAwait(false);
        }

        /// <summary>A <c>DELETE</c> carrying no request body, discarding the response body.</summary>
        internal async Task DeleteAsync(string path, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Delete, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
        }

        /// <summary>A <c>DELETE</c> carrying a JSON body (three signature-required endpoints
        /// take a small body on what used to be a bodyless <c>DELETE</c>), discarding the
        /// response body.</summary>
        internal async Task DeleteWithBodyAsync<TBody>(string path, TBody body, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Delete, Url(path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Token);
            request.Content = JsonContent(body);
            using var response = await _http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
        }

        private static HttpContent JsonContent<T>(T value)
        {
            var json = JsonSerializer.Serialize(value);
            return new StringContent(json, Encoding.UTF8, "application/json");
        }

        /// <summary>
        /// Test-only construction that never touches the network — mirrors
        /// <see cref="Session.ForTesting"/> and the Rust SDK's own test-only construction
        /// pattern. The default server URL is deliberately unroutable: a test that forgets
        /// to stub its handler should fail loudly, not hang.
        /// </summary>
        internal static AccountSession ForTesting(
            HttpClient? http = null,
            string serverUrl = "http://127.0.0.1:1",
            string token = "test-token",
            Guid? identityId = null,
            SigningKeyMaterial? signing = null,
            Profile? profile = null)
        {
            var id = identityId ?? Guid.NewGuid();
            return new AccountSession(
                new Identity(id, DateTimeOffset.UtcNow),
                profile ?? new Profile(id) { DisplayName = "test" },
                http ?? new HttpClient(),
                serverUrl,
                token,
                signing);
        }
    }

    /// <summary>A partial update to an identity's own profile — every field <c>null</c>
    /// means "leave untouched," matching <c>PATCH /me</c>'s own partial-update convention.
    /// Passing <c>""</c> on a three-state field (<see cref="Bio"/>, <see cref="AvatarUrl"/>,
    /// etc.) clears it. Mirrors the Rust SDK's <c>ProfileUpdate</c>.</summary>
    public sealed class AccountProfileUpdate
    {
        /// <summary>New display name (globally-unique handle) — <c>null</c> leaves it
        /// untouched.</summary>
        public string? DisplayName { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? AvatarUrl { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Bio { get; set; }

        /// <summary><c>null</c> leaves it untouched; an empty list clears it.</summary>
        public List<Genre>? FavoriteGenres { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Pronouns { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? BannerUrl { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Status { get; set; }

        /// <summary><c>null</c> leaves it untouched; an empty list clears it.</summary>
        public List<string>? Links { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Timezone { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? ThemeColor { get; set; }

        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Location { get; set; }
    }

    internal sealed class UpdateProfileRequest
    {
        [JsonPropertyName("display_name")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? DisplayName { get; set; }

        [JsonPropertyName("avatar_url")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? AvatarUrl { get; set; }

        [JsonPropertyName("bio")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Bio { get; set; }

        [JsonPropertyName("favorite_genres")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public List<string>? FavoriteGenres { get; set; }

        [JsonPropertyName("pronouns")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Pronouns { get; set; }

        [JsonPropertyName("banner_url")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? BannerUrl { get; set; }

        [JsonPropertyName("status")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Status { get; set; }

        [JsonPropertyName("links")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public List<string>? Links { get; set; }

        [JsonPropertyName("timezone")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Timezone { get; set; }

        [JsonPropertyName("theme_color")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? ThemeColor { get; set; }

        [JsonPropertyName("location")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Location { get; set; }
    }

    public sealed partial class AvalonClient
    {
        /// <summary>
        /// Resumes an already-minted bearer token as an <see cref="AccountSession"/> —
        /// issue #700/#699, mirroring how Hub resumes a persisted session. Holds no local
        /// signing key, so every signature-required method on the result sends its request
        /// unsigned (see <see cref="AccountSession"/>'s own header comment) — use
        /// <see cref="ResumeAccountSessionWithSigningKeyAsync"/> instead when this process
        /// also holds the identity's signing key.
        /// </summary>
        public async Task<AccountSession> ResumeAccountSessionAsync(string token, CancellationToken ct = default)
        {
            var (identity, profile) = await AccountSession.FetchMeAsync(Http, ServerUrl, token, ct).ConfigureAwait(false);
            return new AccountSession(identity, profile, Http, ServerUrl, token, null);
        }

        /// <summary>
        /// Same as <see cref="ResumeAccountSessionAsync"/>, but also resolves
        /// <paramref name="signingKeySeed"/>'s server-side <c>signing_key_id</c> (via
        /// <c>GET /me/devices</c>, matching on public key) so the returned session's
        /// signature-required methods sign automatically — for a caller that persisted both
        /// a bearer token and this identity's 32-byte Ed25519 signing-key seed itself.
        /// </summary>
        public async Task<AccountSession> ResumeAccountSessionWithSigningKeyAsync(string token, byte[] signingKeySeed, CancellationToken ct = default)
        {
            var (identity, profile) = await AccountSession.FetchMeAsync(Http, ServerUrl, token, ct).ConfigureAwait(false);
            var signing = await AccountSession.ResolveSigningKeyAsync(Http, ServerUrl, token, signingKeySeed, ct).ConfigureAwait(false);
            return new AccountSession(identity, profile, Http, ServerUrl, token, signing);
        }
    }
}
