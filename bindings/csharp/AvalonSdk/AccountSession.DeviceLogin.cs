// AvalonClient.StartAccountDeviceLoginAsync (mirrored from crates/sdk/src/
// account/device_login.rs into C#) — an AccountSession-returning counterpart to the
// cross-device pairing flow (crates/server/src/device_pairing.rs). Without this, a client with no
// WebAuthn ceremony surface of its own (a Unity game, a console) had no way to originate a
// first-party account login at all — only ResumeAccountSessionAsync, which needs a token
// minted somewhere else first. The Rust SDK's own integrator-Session equivalent
// (device_login.rs's AvalonClient::login()) has never been ported to this SDK either, so
// this is a fresh port straight from the Rust AccountSession side, not an adaptation of an
// existing C# type.

using System;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>The pending pairing was declined on the approving device. Mirrors the Rust
    /// SDK's <c>SdkError::DeviceLoginDenied</c>.</summary>
    public sealed class AccountDeviceLoginDeniedException : Exception
    {
        public AccountDeviceLoginDeniedException() : base("device pairing was denied")
        {
        }
    }

    /// <summary>The pending pairing expired before ever being approved or denied. Mirrors the
    /// Rust SDK's <c>SdkError::DeviceLoginExpired</c>.</summary>
    public sealed class AccountDeviceLoginExpiredException : Exception
    {
        public AccountDeviceLoginExpiredException() : base("device pairing expired")
        {
        }
    }

    /// <summary>
    /// A pending cross-device pairing for an <see cref="AccountSession"/> login, returned by
    /// <see cref="AvalonClient.StartAccountDeviceLoginAsync"/> — see this file's own header
    /// comment. Mirrors <c>crates/sdk/src/account/device_login.rs::AccountDeviceLogin</c>
    /// field-for-field.
    /// </summary>
    public sealed class AccountDeviceLogin
    {
        private readonly AvalonClient _client;
        private readonly string _deviceCode;
        private readonly int _pollIntervalSeconds;

        private const int DefaultPollIntervalSeconds = 5;
        private const int MaxPollIntervalSeconds = 60;

        /// <summary>Short, human-typeable code to show the user — render it as text, or embed
        /// <see cref="VerificationUri"/> in a QR code.</summary>
        public string UserCode { get; }

        /// <summary>Where the user completes approval on a WebAuthn-capable device (typically
        /// the Hub). Already has <see cref="UserCode"/> embedded as a query param.</summary>
        public string VerificationUri { get; }

        /// <summary>Seconds until this pairing expires if it's never approved.</summary>
        public long ExpiresIn { get; }

        internal AccountDeviceLogin(AvalonClient client, string deviceCode, string userCode, string verificationUri, long expiresIn, int pollIntervalSeconds)
        {
            _client = client;
            _deviceCode = deviceCode;
            UserCode = userCode;
            VerificationUri = verificationUri;
            ExpiresIn = expiresIn;
            _pollIntervalSeconds = pollIntervalSeconds > 0 ? pollIntervalSeconds : DefaultPollIntervalSeconds;
        }

        /// <summary>
        /// Drives <c>POST /auth/device/poll</c> to completion — doubling the poll interval
        /// (capped at <see cref="MaxPollIntervalSeconds"/>) whenever the server answers
        /// <c>slow_down</c>, same backoff shape
        /// <c>crates/sdk/src/account/device_login.rs::AccountDeviceLogin::wait</c> uses.
        /// Resolves to a real <see cref="AccountSession"/> on <c>approved</c> (via
        /// <see cref="AvalonClient.ResumeAccountSessionAsync"/> — the minted token is
        /// ordinary, same as any other login path), or throws
        /// <see cref="AccountDeviceLoginDeniedException"/>/
        /// <see cref="AccountDeviceLoginExpiredException"/> on <c>denied</c>/<c>expired</c>.
        /// </summary>
        public async Task<AccountSession> WaitAsync(CancellationToken ct = default)
        {
            var interval = Math.Max(_pollIntervalSeconds, 1);

            while (true)
            {
                await Task.Delay(TimeSpan.FromSeconds(interval), ct).ConfigureAwait(false);

                using var request = new HttpRequestMessage(HttpMethod.Post, $"{_client.ServerUrl}/auth/device/poll");
                request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _deviceCode);
                using var response = await _client.Http.SendAsync(request, ct).ConfigureAwait(false);
                if (!response.IsSuccessStatusCode)
                {
                    throw Session.ServerError(response.StatusCode);
                }
                var body = await Session.ReadJsonAsync<Avalon.Sdk.Generated.PollPairingResponse>(response, ct).ConfigureAwait(false);

                switch (body.Status)
                {
                    case "pending":
                        continue;
                    case "slow_down":
                        interval = Math.Min(interval * 2, MaxPollIntervalSeconds);
                        continue;
                    case "denied":
                        throw new AccountDeviceLoginDeniedException();
                    case "approved":
                        if (body.Token is null)
                        {
                            throw new InvalidOperationException("approved poll response was missing a token");
                        }
                        return await _client.ResumeAccountSessionAsync(body.Token, ct).ConfigureAwait(false);
                    default:
                        throw new AccountDeviceLoginExpiredException();
                }
            }
        }
    }

    public sealed partial class AvalonClient
    {
        /// <summary>
        /// Starts a cross-device pairing via <c>POST /auth/device/start</c>, resolving
        /// to an <see cref="AccountSession"/> rather than the integrator <see cref="Session"/>.
        /// Use this instead of <see cref="ResumeAccountSessionAsync"/> when this process has no
        /// WebAuthn ceremony surface of its own.
        /// </summary>
        public async Task<AccountDeviceLogin> StartAccountDeviceLoginAsync(CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/auth/device/start");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var body = await Session.ReadJsonAsync<Avalon.Sdk.Generated.StartPairingResponse>(response, ct).ConfigureAwait(false);

            return new AccountDeviceLogin(this, body.DeviceCode, body.UserCode, body.VerificationUri, body.ExpiresIn, (int)body.PollInterval);
        }
    }
}
