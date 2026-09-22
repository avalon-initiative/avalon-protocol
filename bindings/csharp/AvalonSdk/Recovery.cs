// Social recovery request-initiation (issue #201, closed out for C#'s share
// of #741 by #747) — free-standing AvalonClient methods, deliberately not
// Session methods: the whole premise of recovery is the caller has no valid
// session for the identity being recovered yet (this repo's one deliberate
// exception to "every route requires a session," per
// crates/server/src/recovery.rs's own module doc comment). Mirrors
// bindings/ts/src/recovery.ts almost exactly, which is itself this SDK's
// only cross-language reference for this surface (Rust has none yet
// either — see issue #747's own note). See AccountSession.Recovery.cs for
// the already-logged-in side (guardian management, approve/cancel, status)
// this file does not duplicate.
//
// StartRecoveryAsync/FinishRecoveryAsync carry the WebAuthn
// challenge/credential as opaque JSON (System.Text.Json.JsonElement) rather
// than typed WebAuthn structures — this SDK has no WebAuthn ceremony-driving
// code of its own (that lives in a browser/platform authenticator, outside
// this SDK's scope), so a caller integrating this flow is expected to hand
// the challenge to its own WebAuthn client library and pass the resulting
// credential back verbatim, the same shape AccountSession.Passkeys.cs's own
// registration ceremony already leaves to the caller.

using System;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    public sealed partial class AvalonClient
    {
        /// <summary>POST /recovery/requests/start — begins recovering
        /// <paramref name="identityId"/> on a brand-new device by starting a fresh
        /// passkey-registration ceremony for it. No bearer token. Returns a ticket id plus the
        /// raw WebAuthn creation challenge to hand to a platform authenticator; pass both back
        /// to <see cref="FinishRecoveryAsync"/>.</summary>
        public async Task<(Guid TicketId, JsonElement Challenge)> StartRecoveryAsync(
            Guid identityId, string? deviceLabel = null, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/recovery/requests/start");
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.RecoveryStartRequest
                {
                    IdentityId = identityId,
                    DeviceLabel = deviceLabel,
                }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var body = await Session.ReadJsonAsync<Avalon.Sdk.Generated.RecoveryStartResponse>(response, ct).ConfigureAwait(false);
            var challenge = (JsonElement)body.Challenge!;
            return (body.TicketId, challenge);
        }

        /// <summary>POST /recovery/requests/finish — completes the ceremony
        /// <see cref="StartRecoveryAsync"/> began, creating a <c>pending_approvals</c> recovery
        /// request for the identity's guardians to act on. No bearer token.
        /// <paramref name="webauthnCredential"/> is the raw registration-response JSON a
        /// platform authenticator produced from <see cref="StartRecoveryAsync"/>'s
        /// challenge.</summary>
        public async Task<Avalon.Sdk.Generated.RecoveryRequestResponse> FinishRecoveryAsync(
            Guid ticketId, JsonElement webauthnCredential, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/recovery/requests/finish");
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.RecoveryFinishRequest
                {
                    TicketId = ticketId,
                    WebauthnCredential = webauthnCredential,
                }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<Avalon.Sdk.Generated.RecoveryRequestResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>GET /recovery/requests/{id} — public, unauthenticated status read (the
        /// mandatory public time-delay this feature is built around means the fact of an
        /// in-flight recovery must be checkable by anyone, not just the owner or
        /// guardians).</summary>
        public async Task<Avalon.Sdk.Generated.RecoveryRequestResponse> GetRecoveryRequestAsync(Guid requestId, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/recovery/requests/{requestId}");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<Avalon.Sdk.Generated.RecoveryRequestResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /recovery/requests/{id}/finalize — once the guardian threshold is met
        /// and the decision delay has elapsed, mints the new passkey as the identity's active
        /// login credential. No bearer token; idempotent — calling it again after it already
        /// completed just returns the current state.</summary>
        public async Task<Avalon.Sdk.Generated.RecoveryRequestResponse> FinalizeRecoveryRequestAsync(Guid requestId, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/recovery/requests/{requestId}/finalize");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<Avalon.Sdk.Generated.RecoveryRequestResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>GET /identities/{id}/recovery/status — public, unauthenticated: lets an
        /// unfamiliar device check whether a recovery is already in flight for an identity
        /// before starting a duplicate one. <c>null</c> means no active recovery.</summary>
        public async Task<Avalon.Sdk.Generated.RecoveryRequestResponse?> GetIdentityRecoveryStatusAsync(Guid identityId, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/identities/{identityId}/recovery/status");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
#if NET5_0_OR_GREATER
            var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
#else
            var stream = await response.Content.ReadAsStreamAsync().ConfigureAwait(false);
#endif
            return await JsonSerializer.DeserializeAsync<Avalon.Sdk.Generated.RecoveryRequestResponse>(stream, cancellationToken: ct).ConfigureAwait(false);
        }
    }
}
