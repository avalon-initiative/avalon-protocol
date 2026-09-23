// Cross-node login — mirrors crates/sdk/src/cross_node_login.rs,
// itself mirroring device_login's own start-then-poll shape almost exactly: from a caller's
// perspective both flows look identical (POST .../start on this node, show the user a code,
// poll until approved, get back a real Session). The one structural difference: POST
// /auth/cross-node/start returns no verification_uri the way device pairing's would — there
// is no Hub route for cross-node approval yet, just a UserCode and
// RequestingContext to show however this integrator's own UI displays a pairing code.
//
// Also exposes the same-device fast path (AvalonClient.SubmitCrossNodeLoginGrantAsync):
// epic #623's own scope note draws a real distinction between the cross-device dance above
// (needed when the approving human is somewhere that genuinely doesn't hold the identity's
// signing key) and a caller that already does — not the common case for this SDK's usual
// integrator-backend callers (who never hold a *player's* own key), but a real one for a
// deployment that directly controls some identity's key material (e.g. a service/bot
// identity). That caller can mint, sign, and submit a grant in one call, skipping the
// start/poll dance entirely.

using System;
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
    /// <summary>Mirrors <c>SdkError::CrossNodeLoginDenied</c> — the request was explicitly
    /// denied (POST /auth/cross-node/deny).</summary>
    public sealed class CrossNodeLoginDeniedException : Exception
    {
        public CrossNodeLoginDeniedException() : base("cross-node login was denied")
        {
        }
    }

    /// <summary>Mirrors <c>SdkError::CrossNodeLoginExpired</c> — the request's ~10-minute TTL
    /// (<c>crates/server/src/cross_node_login.rs</c>) elapsed before it was approved or
    /// denied.</summary>
    public sealed class CrossNodeLoginExpiredException : Exception
    {
        public CrossNodeLoginExpiredException() : base("cross-node login expired before it was approved")
        {
        }
    }

    /// <summary>
    /// A pending cross-node login, returned by
    /// <see cref="AvalonClient.CrossNodeLoginAsync"/>. Mirrors the Rust SDK's
    /// <c>cross_node_login::CrossNodeLogin</c>.
    /// </summary>
    public sealed class CrossNodeLogin
    {
        /// <summary>Same cap the Rust SDK's own <c>MAX_POLL_INTERVAL_SECONDS</c> documents.</summary>
        private const long MaxPollIntervalSeconds = 60;

        private readonly AvalonClient _client;
        private readonly string _requestCode;
        private long _pollIntervalSeconds;

        internal CrossNodeLogin(
            AvalonClient client,
            string requestCode,
            string userCode,
            string requestingContext,
            long expiresIn,
            long pollIntervalSeconds)
        {
            _client = client;
            _requestCode = requestCode;
            UserCode = userCode;
            RequestingContext = requestingContext;
            ExpiresIn = expiresIn;
            // Same floor the Rust SDK's own DEFAULT_POLL_INTERVAL_SECONDS documents — a server
            // response should always carry its own poll_interval, this is only a fallback.
            _pollIntervalSeconds = pollIntervalSeconds > 0 ? pollIntervalSeconds : 5;
        }

        /// <summary>Short, human-typeable code — show it however this integrator's own UI
        /// displays a pairing code. Unlike a device-pairing verification URI, there's no
        /// ready-made Hub route to point at yet (#639, not built).</summary>
        public string UserCode { get; }

        /// <summary>The requesting node's own context, meant to be shown by whatever eventually
        /// approves this (Hub/mobile-hub, once built) so a human can tell what they're approving
        /// login to.</summary>
        public string RequestingContext { get; }

        /// <summary>Seconds until this request expires if it's never approved.</summary>
        public long ExpiresIn { get; }

        /// <summary>
        /// Drives POST /auth/cross-node/poll to completion — identical backoff shape to the
        /// Rust SDK's <c>CrossNodeLogin::wait</c> (doubling the interval, capped at
        /// <see cref="MaxPollIntervalSeconds"/>, on <c>slow_down</c> rather than treating it as
        /// plain <c>pending</c>). Resolves to a real <see cref="Session"/> on <c>approved</c> by
        /// feeding the minted token through <see cref="AvalonClient.AuthenticateAsync"/> — the
        /// same path a normal login already uses — or throws
        /// <see cref="CrossNodeLoginDeniedException"/>/<see cref="CrossNodeLoginExpiredException"/>
        /// on <c>denied</c>/<c>expired</c>.
        /// </summary>
        public async Task<Session> WaitAsync(CancellationToken ct = default)
        {
            var interval = Math.Max(_pollIntervalSeconds, 1);

            while (true)
            {
                await Task.Delay(TimeSpan.FromSeconds(interval), ct).ConfigureAwait(false);

                using var request = new HttpRequestMessage(HttpMethod.Post, $"{_client.ServerUrl}/auth/cross-node/poll");
                request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _requestCode);
                using var response = await _client.Http.SendAsync(request, ct).ConfigureAwait(false);
                if (!response.IsSuccessStatusCode)
                {
                    throw Session.ServerError(response.StatusCode);
                }
                var body = await Session.ReadJsonAsync<Avalon.Sdk.Generated.PollCrossNodeLoginResponse>(response, ct).ConfigureAwait(false);

                switch (body.Status)
                {
                    case "pending":
                        continue;
                    case "slow_down":
                        interval = Math.Min(interval * 2, MaxPollIntervalSeconds);
                        continue;
                    case "denied":
                        throw new CrossNodeLoginDeniedException();
                    case "approved":
                        // Single-use delivery (cross_node_login.rs's own invariant): a missing
                        // token here would mean this response already lost a consumption race,
                        // which the server surfaces as expired, not approved with no token —
                        // treat it the same as a malformed response.
                        if (body.Token is null)
                        {
                            throw Session.ServerError(System.Net.HttpStatusCode.InternalServerError);
                        }
                        return await _client.AuthenticateAsync(body.Token, ct).ConfigureAwait(false);
                    default:
                        // "expired" and anything unrecognized both mean this request is done and
                        // will never resolve to a session.
                        throw new CrossNodeLoginExpiredException();
                }
            }
        }
    }

    public sealed partial class AvalonClient
    {
        /// <summary>Starts a cross-node login via POST /auth/cross-node/start
        /// against this client's own <see cref="AvalonConfig.ServerUrl"/> — the node being
        /// logged into, which may not be the identity's "home" node at all.</summary>
        public async Task<CrossNodeLogin> CrossNodeLoginAsync(CancellationToken ct = default)
        {
            // No idempotency key: a retried start would just mint a second, independent request
            // rather than replay the first, same reasoning as every other unkeyed write.
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/auth/cross-node/start");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var body = await Session.ReadJsonAsync<Avalon.Sdk.Generated.StartCrossNodeLoginResponse>(response, ct).ConfigureAwait(false);

            return new CrossNodeLogin(
                this,
                body.RequestCode,
                body.UserCode,
                body.RequestingContext,
                body.ExpiresIn,
                body.PollInterval);
        }

        /// <summary>The exact bytes a <c>CrossNodeLoginGrant</c>'s signature covers — must match
        /// <c>avalon_protocol::cross_node_login::signing_bytes</c> exactly. This SDK defines its
        /// own copy rather than depending on the server's private construction, same posture as
        /// every other signed request in this repo (see <c>Achievements.cs</c>'s own
        /// <c>AttestationSigningBytes</c>).</summary>
        private static byte[] CrossNodeLoginSigningBytes(
            Guid identityId,
            Guid signingKeyId,
            string destinationBaseUrl,
            string requestingContext,
            Guid nonce,
            DateTimeOffset issuedAt,
            DateTimeOffset expiresAt)
        {
            var text =
                $"avalon:cross-node-login:v1:{identityId}:{signingKeyId}:{destinationBaseUrl}:" +
                $"{requestingContext}:{nonce}:{issuedAt.ToUnixTimeSeconds()}:{expiresAt.ToUnixTimeSeconds()}";
            return Encoding.UTF8.GetBytes(text);
        }

        /// <summary>Pure-managed Ed25519 via BouncyCastle, same library (and Unity/IL2CPP-safe
        /// reasoning) <c>Achievements.cs</c>'s own <c>SignWithIssuerKey</c> already
        /// establishes — this is the identity-level counterpart, signing with the *player's*
        /// own event-signing key seed rather than an integrator's issuer key.</summary>
        private static byte[] SignWithIdentityKey(byte[] signingKeySeed, byte[] message)
        {
            var privateKey = new Ed25519PrivateKeyParameters(signingKeySeed, 0);
            var signer = new Ed25519Signer();
            signer.Init(true, privateKey);
            signer.BlockUpdate(message, 0, message.Length);
            return signer.GenerateSignature();
        }

        private static string ToLowerHex(byte[] bytes)
        {
            var sb = new StringBuilder(bytes.Length * 2);
            foreach (var b in bytes)
            {
                sb.Append(b.ToString("x2"));
            }
            return sb.ToString();
        }

        /// <summary>
        /// The same-device fast path (see this file's header comment): mints, signs, and
        /// submits a <c>CrossNodeLoginGrant</c> directly against this client's own
        /// <see cref="AvalonConfig.ServerUrl"/>, for a caller that already holds
        /// <paramref name="identityId"/>'s real 32-byte Ed25519 event-signing key seed
        /// (<paramref name="signingKeySeed"/>, identified by <paramref name="signingKeyId"/>) —
        /// skipping <see cref="CrossNodeLoginAsync"/>/<see cref="CrossNodeLogin.WaitAsync"/>
        /// entirely. Resolves to a real <see cref="Session"/> the same way <c>WaitAsync</c>
        /// does, via <see cref="AuthenticateAsync"/>.
        /// </summary>
        public async Task<Session> SubmitCrossNodeLoginGrantAsync(
            Guid identityId,
            Guid signingKeyId,
            byte[] signingKeySeed,
            CancellationToken ct = default)
        {
            var issuedAt = DateTimeOffset.UtcNow;
            // Same TTL avalon_protocol::cross_node_login::DEFAULT_TTL_SECONDS documents — this
            // SDK defines its own copy for the same reason CrossNodeLoginSigningBytes does.
            var expiresAt = issuedAt.AddSeconds(60);
            var nonce = Guid.NewGuid();
            var destinationBaseUrl = ServerUrl;
            // No Hub-driven context yet to show a human — the destination itself is the
            // most honest context this caller can supply on its own behalf.
            var requestingContext = destinationBaseUrl;

            var signingBytes = CrossNodeLoginSigningBytes(
                identityId, signingKeyId, destinationBaseUrl, requestingContext, nonce, issuedAt, expiresAt);
            var signature = SignWithIdentityKey(signingKeySeed, signingBytes);

            var grant = new Avalon.Sdk.Generated.CrossNodeLoginGrant
            {
                IdentityId = identityId,
                SigningKeyId = signingKeyId,
                DestinationBaseUrl = destinationBaseUrl,
                RequestingContext = requestingContext,
                Nonce = nonce,
                IssuedAt = issuedAt,
                ExpiresAt = expiresAt,
                Signature = ToLowerHex(signature),
            };

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/auth/cross-node/submit");
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.SubmitGrantRequest { UserCode = null, Grant = grant }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var body = await Session.ReadJsonAsync<Avalon.Sdk.Generated.SubmitGrantResponse>(response, ct).ConfigureAwait(false);
            if (body.Token is null)
            {
                throw Session.ServerError(System.Net.HttpStatusCode.InternalServerError);
            }
            return await AuthenticateAsync(body.Token, ct).ConfigureAwait(false);
        }

        // --- The approving device's own half ---
        //
        // CrossNodeLoginAsync/WaitAsync/SubmitCrossNodeLoginGrantAsync above are
        // the *requester's* side (the node being logged into). LookupCrossNodeLoginAsync/
        // DenyCrossNodeLoginAsync are the *approver's* side — whatever surface shows a human
        // the pairing code (no Hub route for this yet) looks the request up by its
        // short user code and can explicitly deny it. Neither call carries a bearer token:
        // the user code itself is the only credential, same shape device pairing's own
        // deny/resolve endpoints already use.

        /// <summary>GET /auth/cross-node/lookup?user_code=… — resolves a short pairing code
        /// into the full pending request (status, requesting context, whether the requesting
        /// node is a verified integrator) for an approval screen to render. Public — the user
        /// code itself is the only proof needed to look up its own request.</summary>
        public async Task<Avalon.Sdk.Generated.LookupCrossNodeLoginResponse> LookupCrossNodeLoginAsync(
            string userCode, CancellationToken ct = default)
        {
            var url = $"{ServerUrl}/auth/cross-node/lookup?user_code={Uri.EscapeDataString(userCode)}";
            using var request = new HttpRequestMessage(HttpMethod.Get, url);
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<Avalon.Sdk.Generated.LookupCrossNodeLoginResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /auth/cross-node/deny — explicitly denies the pending request named
        /// by <paramref name="userCode"/>; <see cref="CrossNodeLogin.WaitAsync"/> on the
        /// requesting side then surfaces this as <see cref="CrossNodeLoginDeniedException"/>.
        /// Only a currently-<c>pending</c>, unexpired request can be denied.</summary>
        public async Task DenyCrossNodeLoginAsync(string userCode, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/auth/cross-node/deny");
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.UserCodeRequest { UserCode = userCode }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
        }
    }
}
