// Integrator connect/consent (issue #27/#83) on AccountSession — an identity granting or
// revoking its own consent to an integrator, not anything the integrator does on its own
// behalf. Mirrors crates/sdk/src/account/integrations.rs.

using System;
using System.Collections.Generic;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>The result of a successful <see cref="AccountSession.ConnectIntegratorAsync"/>
    /// call. Mirrors the Rust SDK's <c>account::integrations::IntegratorConnection</c>.</summary>
    public sealed class IntegratorConnection
    {
        [JsonPropertyName("binding_id")]
        public Guid BindingId { get; set; }

        [JsonPropertyName("integrator_id")]
        public Guid IntegratorId { get; set; }

        [JsonPropertyName("established_at")]
        public DateTimeOffset EstablishedAt { get; set; }

        /// <summary>Every capability actually granted (may be a subset of what was
        /// requested).</summary>
        [JsonPropertyName("granted_capabilities")]
        public List<string> GrantedCapabilities { get; set; } = new List<string>();
    }

    /// <summary>One currently-active grant within a <see cref="MyConnection"/>. Mirrors the
    /// Rust SDK's <c>account::integrations::ConnectionGrant</c>.</summary>
    public sealed class ConnectionGrant
    {
        [JsonPropertyName("capability")]
        public string Capability { get; set; } = "";

        [JsonPropertyName("granted_at")]
        public DateTimeOffset GrantedAt { get; set; }
    }

    /// <summary>One of the caller's own active integrator connections (bindings). Mirrors
    /// the Rust SDK's <c>account::integrations::MyConnection</c>.</summary>
    public sealed class MyConnection
    {
        [JsonPropertyName("binding_id")]
        public Guid BindingId { get; set; }

        [JsonPropertyName("integrator_id")]
        public Guid IntegratorId { get; set; }

        [JsonPropertyName("slug")]
        public string Slug { get; set; } = "";

        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        [JsonPropertyName("established_at")]
        public DateTimeOffset EstablishedAt { get; set; }

        /// <summary>Every currently-active grant.</summary>
        [JsonPropertyName("grants")]
        public List<ConnectionGrant> Grants { get; set; } = new List<ConnectionGrant>();
    }

    internal sealed class ConnectRequest
    {
        [JsonPropertyName("capabilities")]
        public List<string> Capabilities { get; set; } = new List<string>();

        [JsonPropertyName("signing_key_id")]
        public Guid? SigningKeyId { get; set; }

        [JsonPropertyName("signature")]
        public string? Signature { get; set; }
    }

    public sealed partial class AccountSession
    {
        /// <summary><c>POST /integrations/{slug}/connect</c> — the one call that hands a
        /// third party standing permission over this identity's data going forward; always
        /// signed (<c>integration.connect</c>, <c>[slug, capabilities comma-joined in
        /// request order]</c>). Idempotent: reconnecting to an already-bound integrator
        /// doesn't duplicate the binding, but does still grant any newly-approved
        /// capabilities.</summary>
        public async Task<IntegratorConnection> ConnectIntegratorAsync(string slug, IReadOnlyList<string> capabilities, CancellationToken ct = default)
        {
            var joined = string.Join(",", capabilities);
            var (signingKeyId, signature) = Sign("integration.connect", slug, joined);
            return await PostAsync<ConnectRequest, IntegratorConnection>(
                $"/integrations/{slug}/connect",
                new ConnectRequest { Capabilities = new List<string>(capabilities), SigningKeyId = signingKeyId, Signature = signature },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>DELETE /integrations/{slug}/connect</c>. Not signature-required
        /// (revocation only narrows what an integrator can do).</summary>
        public async Task DisconnectIntegratorAsync(string slug, CancellationToken ct = default) =>
            await DeleteAsync($"/integrations/{slug}/connect", ct).ConfigureAwait(false);

        /// <summary><c>DELETE /integrations/{slug}/grants/{capability}</c>. Not
        /// signature-required, same reasoning as <see cref="DisconnectIntegratorAsync"/>.</summary>
        public async Task RevokeGrantAsync(string slug, string capability, CancellationToken ct = default) =>
            await DeleteAsync($"/integrations/{slug}/grants/{capability}", ct).ConfigureAwait(false);

        /// <summary><c>GET /me/connections</c> — every integrator this identity has
        /// currently consented to, and what it granted each one.</summary>
        public async Task<IReadOnlyList<MyConnection>> MyConnectionsAsync(CancellationToken ct = default) =>
            await GetAsync<List<MyConnection>>("/me/connections", ct).ConfigureAwait(false);
    }
}
