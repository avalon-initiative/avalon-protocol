// Integrator Space schema publication, schema-to-schema mapping, and
// instance-data publication/read (issue #255/#384/#491, closed out for C#'s
// share of #741 by #745) — mirrors crates/server/src/integrator_schemas.rs,
// integrator_schema_mappings.rs, and integrator_data.rs almost exactly.
//
// Writes (publish/delete) are challenge-authenticated only, same posture
// every other slug-owner write in this SDK takes (see Session.cs's own
// AttachIntegratorAuthAsync doc comment) — no user capability grant,
// since these are the integrator declaring/publishing its own data, not
// acting on a specific player's behalf. Reads are public, unauthenticated.

using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    public sealed partial class Session
    {
        // --- Schema versions (issue #255/#384/#745) ---

        /// <summary>POST /integrations/{slug}/schemas — publishes the next version of this
        /// integrator's own data schema. Always an insert, never an update to an existing
        /// version (schema versions are immutable once published).</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorSchemaVersionResponse> PublishSchemaVersionAsync(
            string protoSource, string? defaultVisibility = null, IDictionary<string, string>? fieldVisibility = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/schemas");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.PublishIntegratorSchemaVersionRequest
            {
                ProtoSource = protoSource,
                // The server's own fields are plain `String`/`BTreeMap` with a `#[serde(default
                // = ...)]`, not `Option` — a literal JSON `null` fails to deserialize, so these
                // always send a real value/collection, matching the server's own defaults.
                DefaultVisibility = defaultVisibility ?? "public",
                Field_visibility = fieldVisibility ?? new Dictionary<string, string>(),
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.IntegratorSchemaVersionResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>GET /integrations/{slug}/schemas — every published version for
        /// <paramref name="slug"/>, oldest first. Public, unauthenticated.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.IntegratorSchemaVersionResponse>> ListSchemaVersionsAsync(
            string slug, CancellationToken ct = default) =>
            await GetJsonAsync<List<Avalon.Sdk.Generated.IntegratorSchemaVersionResponse>>(
                $"{ServerUrl}/integrations/{slug}/schemas", ct).ConfigureAwait(false)
            ?? new List<Avalon.Sdk.Generated.IntegratorSchemaVersionResponse>();

        /// <summary>GET /integrations/{slug}/schemas/{version} — one published version,
        /// verbatim. Public, unauthenticated.</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorSchemaVersionResponse> GetSchemaVersionAsync(
            string slug, uint version, CancellationToken ct = default) =>
            await GetJsonAsync<Avalon.Sdk.Generated.IntegratorSchemaVersionResponse>(
                $"{ServerUrl}/integrations/{slug}/schemas/{version}", ct).ConfigureAwait(false);

        // --- Schema-to-schema mappings (issue #491/#745) ---

        /// <summary>POST /integrations/{slug}/mappings — documents a correspondence between
        /// two of this integrator's own already-published schema versions. Never interpreted
        /// or executed server-side or by this SDK — pure documentation.</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorSchemaMappingResponse> PublishMappingAsync(
            string fromSchemaId, string toSchemaId, string? description = null,
            IDictionary<string, string>? fieldCorrespondence = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/mappings");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.PublishIntegratorSchemaMappingRequest
            {
                FromSchemaId = fromSchemaId,
                ToSchemaId = toSchemaId,
                // Same "plain String/BTreeMap, not Option" reasoning as
                // PublishSchemaVersionAsync's own DefaultVisibility/Field_visibility.
                Description = description ?? "",
                Field_correspondence = fieldCorrespondence ?? new Dictionary<string, string>(),
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.IntegratorSchemaMappingResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>GET /integrations/{slug}/mappings — every published mapping for
        /// <paramref name="slug"/>, oldest first. Public, unauthenticated.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.IntegratorSchemaMappingResponse>> ListMappingsAsync(
            string slug, CancellationToken ct = default) =>
            await GetJsonAsync<List<Avalon.Sdk.Generated.IntegratorSchemaMappingResponse>>(
                $"{ServerUrl}/integrations/{slug}/mappings", ct).ConfigureAwait(false)
            ?? new List<Avalon.Sdk.Generated.IntegratorSchemaMappingResponse>();

        /// <summary>GET /integrations/{slug}/mappings/{seq} — one published mapping, verbatim.
        /// Public, unauthenticated.</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorSchemaMappingResponse> GetMappingAsync(
            string slug, uint seq, CancellationToken ct = default) =>
            await GetJsonAsync<Avalon.Sdk.Generated.IntegratorSchemaMappingResponse>(
                $"{ServerUrl}/integrations/{slug}/mappings/{seq}", ct).ConfigureAwait(false);

        // --- Instance data (issue #384/#745) ---

        /// <summary>POST /integrations/{slug}/schemas/{version}/data — publishes (or
        /// supersedes) this integrator's own instance data about <paramref name="subject"/>
        /// against one of its own published schema versions. The subject must have an active
        /// binding to this integrator (the user's own consent) — enforced server-side, same
        /// posture as attestation issuance.</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorDataInstanceResponse> PublishInstanceAsync(
            uint version, Guid subject, object instance, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/schemas/{version}/data");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.PublishInstanceRequest { Subject = subject, Instance = instance });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.IntegratorDataInstanceResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>DELETE /integrations/{slug}/schemas/{version}/data/{subject} (issue #533) —
        /// an append-only tombstone: the original instance's data is never mutated, only marked
        /// deleted. The original publish event stays visible in raw ledger history either
        /// way.</summary>
        public async Task DeleteInstanceAsync(
            uint version, Guid subject, string? reasonCode = null, string? reason = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Delete, $"{ServerUrl}/integrations/{IntegratorSlug}/schemas/{version}/data/{subject}");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            // ReasonCode is a plain String server-side (`#[serde(default = ...)]`, not
            // `Option`) — a literal JSON `null` fails to deserialize, so this always sends a
            // real value, matching the server's own "deleted" default.
            request.Content = JsonContent(new Avalon.Sdk.Generated.DeleteInstanceRequest { ReasonCode = reasonCode ?? "deleted", Reason = reason });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
        }

        /// <summary>GET /identities/{id}/integrator-data (issue #384) — every current
        /// (non-superseded, non-deleted) instance published about <paramref name="identityId"/>
        /// across every integrator/schema, each already filtered to only the fields its
        /// schema's own visibility policy currently makes visible. Public, unauthenticated —
        /// publishing instance data is itself the opt-in.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.VisibleIntegratorDataInstanceResponse>> GetIdentityIntegratorDataAsync(
            Guid identityId, CancellationToken ct = default) =>
            await GetJsonAsync<List<Avalon.Sdk.Generated.VisibleIntegratorDataInstanceResponse>>(
                $"{ServerUrl}/identities/{identityId}/integrator-data", ct).ConfigureAwait(false)
            ?? new List<Avalon.Sdk.Generated.VisibleIntegratorDataInstanceResponse>();
    }
}
