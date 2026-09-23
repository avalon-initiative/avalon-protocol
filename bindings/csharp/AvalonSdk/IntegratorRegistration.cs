// Integrator registration and per-network issuer-key registration — mirrors
// crates/server/src/integrators.rs's register_integrator and
// crates/server/src/issuer_registration.rs's register_issuer/
// create_registration_challenge. Both live on AvalonClient, not Session:
// there is no identity session yet at the point an integrator is
// registering itself or proving possession of a raw signing key — the same
// reasoning CrossNodeLogin.cs's own AvalonClient-level methods already
// establish.
//
// avalon-cli's own register-integrator command (crates/cli) is the
// reference for the request/response shape this mirrors.

using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    public sealed partial class AvalonClient
    {
        /// <summary>POST /integrations — registers a brand-new integrator (game, app, or
        /// service — <paramref name="category"/> is <c>"integrator"</c>/<c>"app"</c>/
        /// <c>"service"</c>; omit for the server's own <c>"integrator"</c> default) with its
        /// first issuer key. No auth: this is the entry point before any credential exists.
        /// The returned <c>IntegratorResponse.Credential.KeyId</c> is what
        /// <see cref="AvalonConfig.IntegratorCredentialKeyId"/> should be constructed
        /// with.</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorResponse> RegisterIntegratorAsync(
            string slug,
            string name,
            string ownerName,
            string initialKeyAlgorithm,
            string initialKeyPublicKeyBase64,
            System.Collections.Generic.IReadOnlyList<string>? requestedCapabilities = null,
            string? category = null,
            CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations");
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.CreateIntegratorRequest
                {
                    Slug = slug,
                    Name = name,
                    OwnerName = ownerName,
                    // The server's own field is `#[serde(default)] Vec<String>`, not an
                    // `Option` — a literal JSON `null` fails to deserialize (unlike `Category`
                    // below, which really is optional server-side), so this always sends a
                    // real (possibly empty) array.
                    RequestedCapabilities = new System.Collections.Generic.List<string>(requestedCapabilities ?? Array.Empty<string>()),
                    InitialKey = new Avalon.Sdk.Generated.InitialKeyRequest
                    {
                        Algorithm = initialKeyAlgorithm,
                        PublicKey = initialKeyPublicKeyBase64,
                    },
                    Category = category,
                }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<Avalon.Sdk.Generated.IntegratorResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /issuers/registration-challenge — the first half of issuer network
        /// registration: a short-lived, single-use nonce for
        /// <see cref="RegisterIssuerAsync"/> to sign as proof of possession of
        /// <paramref name="issuerPubkeyBase64"/>. No auth: obtaining a challenge proves
        /// nothing by itself.</summary>
        public async Task<Avalon.Sdk.Generated.RegistrationChallengeResponse> CreateRegistrationChallengeAsync(
            string issuerPubkeyBase64, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/issuers/registration-challenge");
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.RegistrationChallengeRequest { IssuerPubkey = issuerPubkeyBase64 }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<Avalon.Sdk.Generated.RegistrationChallengeResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /issuers/register — admits
        /// <paramref name="signingKeySeed"/>'s public half to write on
        /// <paramref name="declaredNetworkId"/> under <paramref name="issuerRef"/>. Drives the
        /// whole challenge/proof-of-possession round trip itself
        /// (<see cref="CreateRegistrationChallengeAsync"/> then this call), signing
        /// <c>"avalon:issuer.registered:v1:&lt;issuer_ref&gt;:&lt;declared_network_id&gt;:&lt;nonce&gt;"</c>
        /// — must match <c>crates/server/src/issuer_registration.rs::proof_of_possession_message</c>
        /// exactly. Idempotent server-side: re-registering an already-registered key just
        /// updates its <c>issuer_ref</c>.</summary>
        public async Task<Avalon.Sdk.Generated.IssuerRegistrationResponse> RegisterIssuerAsync(
            byte[] signingKeySeed, string issuerRef, string declaredNetworkId, CancellationToken ct = default)
        {
            var privateKey = new Org.BouncyCastle.Crypto.Parameters.Ed25519PrivateKeyParameters(signingKeySeed, 0);
            var publicKey = privateKey.GeneratePublicKey().GetEncoded();
            var issuerPubkeyBase64 = Convert.ToBase64String(publicKey);

            var challenge = await CreateRegistrationChallengeAsync(issuerPubkeyBase64, ct).ConfigureAwait(false);
            var nonce = Convert.FromBase64String(challenge.Nonce);
            var message = Encoding.UTF8.GetBytes(
                $"avalon:issuer.registered:v1:{issuerRef}:{declaredNetworkId}:{Convert.ToBase64String(nonce)}");

            var signer = new Org.BouncyCastle.Crypto.Signers.Ed25519Signer();
            signer.Init(true, privateKey);
            signer.BlockUpdate(message, 0, message.Length);
            var signature = signer.GenerateSignature();

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/issuers/register");
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.RegisterIssuerRequest
                {
                    IssuerPubkey = issuerPubkeyBase64,
                    IssuerRef = issuerRef,
                    DeclaredNetworkId = declaredNetworkId,
                    ChallengeId = challenge.ChallengeId,
                    ProofOfPossessionSignature = Convert.ToBase64String(signature),
                }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            return await Session.ReadJsonAsync<Avalon.Sdk.Generated.IssuerRegistrationResponse>(response, ct).ConfigureAwait(false);
        }
    }

    public sealed partial class Session
    {
        // --- Integrator directory reads — all public, unauthenticated. ---

        /// <summary>GET /integrations?q=&amp;sort=&amp;limit=&amp;cursor= — paginated integrator
        /// discovery. <paramref name="sort"/> is <c>"newest"</c> (default) or
        /// <c>"name"</c>.</summary>
        public async Task<Avalon.Sdk.Generated.ListIntegratorsResponse> ListIntegratorsAsync(
            string? q = null, string? sort = null, long? limit = null, Guid? cursor = null, CancellationToken ct = default)
        {
            var parts = new System.Collections.Generic.List<string>();
            if (!string.IsNullOrEmpty(q)) parts.Add($"q={Uri.EscapeDataString(q)}");
            if (!string.IsNullOrEmpty(sort)) parts.Add($"sort={Uri.EscapeDataString(sort)}");
            if (limit.HasValue) parts.Add($"limit={limit.Value}");
            if (cursor.HasValue) parts.Add($"cursor={cursor.Value}");
            var qs = parts.Count == 0 ? "" : "?" + string.Join("&", parts);
            return await GetJsonAsync<Avalon.Sdk.Generated.ListIntegratorsResponse>($"{ServerUrl}/integrations{qs}", ct).ConfigureAwait(false);
        }

        /// <summary>GET /integrations/{slug} — one integrator's public registration
        /// facts.</summary>
        public async Task<Avalon.Sdk.Generated.IntegratorPublicResponse> GetIntegratorAsync(string slug, CancellationToken ct = default) =>
            await GetJsonAsync<Avalon.Sdk.Generated.IntegratorPublicResponse>($"{ServerUrl}/integrations/{slug}", ct).ConfigureAwait(false);

        /// <summary>GET /integrations/{slug}/keys — an issuer's full key history (any role,
        /// any status), oldest first. Public: public keys are already public by
        /// definition.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.IssuerKeyResponse>> ListIssuerKeysAsync(string slug, CancellationToken ct = default) =>
            await GetJsonAsync<System.Collections.Generic.List<Avalon.Sdk.Generated.IssuerKeyResponse>>(
                $"{ServerUrl}/integrations/{slug}/keys", ct).ConfigureAwait(false)
            ?? new System.Collections.Generic.List<Avalon.Sdk.Generated.IssuerKeyResponse>();

        /// <summary>GET /integrations/whoami — proves this session's own
        /// <see cref="IntegratorSlug"/>/<see cref="SigningKey"/> authenticate as a real,
        /// currently-valid integrator key. Not a capability-bearing endpoint on its own; exists
        /// to let an integration sanity-check its own credentials.</summary>
        public async Task<Guid> IntegratorWhoamiAsync(CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/integrations/whoami");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var body = await ReadJsonAsync<Avalon.Sdk.Generated.IntegratorWhoamiResponse>(response, ct).ConfigureAwait(false);
            return body.IntegratorId;
        }

        /// <summary>POST /integrations/{slug}/keys — adds a new key to this
        /// integrator's own key set. Requires this session's configured key to itself be a
        /// currently-valid <b>root</b> key (<paramref name="role"/> of the key being added may
        /// be <c>"root"</c> or <c>"operational"</c> — that's independent of what authorizes
        /// adding it).</summary>
        public async Task<Avalon.Sdk.Generated.IssuerKeyResponse> AddIssuerKeyAsync(
            string algorithm, string publicKeyBase64, string role, string? purpose = null,
            DateTimeOffset? validUntil = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/keys");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.AddIssuerKeyRequest
            {
                Algorithm = algorithm,
                PublicKey = publicKeyBase64,
                Role = role,
                // Purpose is a plain String server-side (`#[serde(default = ...)]`, not
                // `Option`) — a literal JSON `null` fails to deserialize, so this always sends
                // a real value, matching the server's own "attestation" default.
                Purpose = purpose ?? "attestation",
                ValidUntil = validUntil,
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.IssuerKeyResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /integrations/{slug}/keys/{key_id}/revoke — same
        /// root-key requirement as <see cref="AddIssuerKeyAsync"/>. Revoking an already-revoked
        /// or nonexistent key is an error, not a silent no-op.</summary>
        public async Task<Avalon.Sdk.Generated.IssuerKeyResponse> RevokeIssuerKeyAsync(
            Guid keyId, string? reason = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/keys/{keyId}/revoke");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.RevokeIssuerKeyRequest { Reason = reason });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.IssuerKeyResponse>(response, ct).ConfigureAwait(false);
        }
    }
}
