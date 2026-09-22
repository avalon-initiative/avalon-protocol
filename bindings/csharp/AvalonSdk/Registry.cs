// Public recognition relationships and the Integrator Registry's derived
// metrics (issue #89/#95/#96/#261, closed out for C#'s share of #741 by
// #748) — mirrors crates/server/src/recognitions.rs and registry.rs.
//
// publish_recognition/revoke_recognition are challenge-authenticated only
// (an integrator declaring/withdrawing its own policy about another
// integrator, not touching user data) — no user capability grant, same
// posture Session.cs's own AttachIntegratorAuthAsync doc comment
// describes. Every read here is public and unauthenticated.

using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    public sealed partial class Session
    {
        /// <summary>POST /integrations/{slug}/recognitions — publishes (or updates) this
        /// integrator's recognition of <paramref name="recognizedSlug"/>'s claims, for
        /// <paramref name="scope"/>. Upserted, not append-only: republishing updates the
        /// existing row's scope/timestamp in place and clears any prior revocation.</summary>
        public async Task<Avalon.Sdk.Generated.RecognitionResponse> PublishRecognitionAsync(
            string recognizedSlug, IReadOnlyList<string> scope, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/recognitions");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.PublishRecognitionRequest
            {
                RecognizedSlug = recognizedSlug,
                Scope = new List<string>(scope),
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.RecognitionResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /integrations/{slug}/recognitions/revoke — marks this integrator's
        /// recognition of <paramref name="recognizedSlug"/> revoked (the row is kept, not
        /// deleted — "A used to recognize B, then stopped" stays visible). A no-op, not an
        /// error, if no recognition was ever published; returns whether anything was actually
        /// revoked.</summary>
        public async Task<bool> RevokeRecognitionAsync(string recognizedSlug, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/recognitions/revoke");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.RevokeRecognitionRequest { RecognizedSlug = recognizedSlug });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var body = await ReadJsonAsync<RevokeRecognitionResultResponse>(response, ct).ConfigureAwait(false);
            return body.Revoked;
        }

        /// <summary>GET /integrations/{slug}/recognitions — every integrator <paramref
        /// name="slug"/> currently, actively recognizes. Public, unauthenticated.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.RecognitionResponse>> ListRecognitionsAsync(
            string slug, CancellationToken ct = default) =>
            await GetJsonAsync<List<Avalon.Sdk.Generated.RecognitionResponse>>(
                $"{ServerUrl}/integrations/{slug}/recognitions", ct).ConfigureAwait(false)
            ?? new List<Avalon.Sdk.Generated.RecognitionResponse>();

        /// <summary>GET /integrations/{slug}/recognized-by — every integrator that currently,
        /// actively recognizes <paramref name="slug"/>. Public, unauthenticated.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.RecognitionResponse>> ListRecognizedByAsync(
            string slug, CancellationToken ct = default) =>
            await GetJsonAsync<List<Avalon.Sdk.Generated.RecognitionResponse>>(
                $"{ServerUrl}/integrations/{slug}/recognized-by", ct).ConfigureAwait(false)
            ?? new List<Avalon.Sdk.Generated.RecognitionResponse>();

        /// <summary>GET /integrations/{slug}/registry (issue #261) — four durable-derived
        /// metrics about an integrator, each carrying its own definition/class label and
        /// whether it's exact or floor-coarsened for a small cohort (issue #96). No composite
        /// score, no ranking. Public, unauthenticated.</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorRegistryResponse> GetIntegratorRegistryAsync(
            string slug, CancellationToken ct = default) =>
            await GetJsonAsync<Avalon.Sdk.Generated.IntegratorRegistryResponse>(
                $"{ServerUrl}/integrations/{slug}/registry", ct).ConfigureAwait(false);

        /// <summary>Hand-rolled: <c>POST /integrations/{slug}/recognitions/revoke</c> returns
        /// a bare <c>{ "revoked": bool }</c> the server never gave a named schema type — see
        /// its own doc comment (<c>responses((status = 200, description = "{ \"revoked\":
        /// bool }"))</c>).</summary>
        private sealed class RevokeRecognitionResultResponse
        {
            [System.Text.Json.Serialization.JsonPropertyName("revoked")]
            public bool Revoked { get; set; }
        }
    }
}
