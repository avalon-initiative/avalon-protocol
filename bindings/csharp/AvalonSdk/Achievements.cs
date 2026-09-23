// Achievements — read a user's own attestation
// history and issue a new attestation on this integrator's own behalf.
// Mirrors crates/sdk/src/achievements.rs's single-claim (non-bulk,
// non-revocation) surface; bulk issuance and revocation are
// separate, later Rust-side tickets with no C# equivalent yet — the
// invariant that this SDK never exposes a method the Rust SDK doesn't have,
// and vice versa, is about keeping the two surfaces from silently diverging,
// not about building every Rust method on day one.
//
// Reading (GetAchievementsAsync) uses the identity's own bearer token
// against GET /me/achievements — authenticity/validity come back as the
// server computed them; recognition is deliberately never present
// (authenticity, validity, and recognition are three separate questions —
// this SDK never collapses them into one boolean).
//
// Issuing (IssueAchievementAsync) needs this integrator's own signing key —
// the server never sees it, only a detached signature. Two independent
// proofs go out: an ephemeral challenge-response proving this key is making
// the HTTP call right now, and a separate signature embedded in the request
// body over the attestation's own canonical bytes.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;
using Org.BouncyCastle.Crypto.Parameters;
using Org.BouncyCastle.Crypto.Signers;

namespace Avalon.Sdk
{
    /// <summary>Mirrors <c>avalon_chain::attestations::Authenticity</c> at the wire level —
    /// whether the embedded signature actually verifies against one of the issuer's keys.
    /// A plain data class rather than a discriminated union: <see cref="Status"/> carries the
    /// tag ("authentic" / "not_authentic") and only the field that status defines is
    /// populated.</summary>
    public sealed class Authenticity
    {
        [JsonPropertyName("status")]
        public string Status { get; set; } = "";

        /// <summary>Which of the issuer's keys verified it — present only when
        /// <see cref="IsAuthentic"/>.</summary>
        [JsonPropertyName("key_id")]
        public string? KeyId { get; set; }

        /// <summary>Why it doesn't verify — present only when not authentic. Never used to make
        /// an authorization decision, only to explain a rejection to a developer.</summary>
        [JsonPropertyName("reason")]
        public string? Reason { get; set; }

        public bool IsAuthentic => Status == "authentic";
    }

    /// <summary>Mirrors <c>avalon_protocol::achievements::Validity</c> at the wire level —
    /// whether an attestation is still in force (not revoked).</summary>
    public sealed class Validity
    {
        [JsonPropertyName("status")]
        public string Status { get; set; } = "";

        /// <summary>Present only when not valid.</summary>
        [JsonPropertyName("reason")]
        public string? Reason { get; set; }

        public bool IsValid => Status == "valid";
    }

    /// <summary>One entry in an attestation's history ("issued", plus "revoked" if applicable).
    /// Mirrors <c>AttestationHistoryEntry</c> server-side.</summary>
    public sealed class AttestationHistoryEntry
    {
        [JsonPropertyName("event")]
        public string Event { get; set; } = "";

        [JsonPropertyName("at")]
        public DateTimeOffset At { get; set; }

        [JsonPropertyName("reason_code")]
        public string? ReasonCode { get; set; }

        [JsonPropertyName("reason")]
        public string? Reason { get; set; }
    }

    /// <summary>One attestation from the caller's own history, with its computed authenticity
    /// and validity attached — deliberately no combined "trusted" field; see this file's own
    /// header comment. Mirrors <c>avalon_sdk::achievements::VerifiedAttestation</c>.
    /// </summary>
    public sealed class VerifiedAttestation
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        /// <summary>The issuer that issued it, e.g. "game:&lt;slug&gt;".</summary>
        [JsonPropertyName("issuer")]
        public string Issuer { get; set; } = "";

        [JsonPropertyName("subject")]
        public Guid Subject { get; set; }

        /// <summary>The achievement definition this attestation claims, e.g.
        /// "game:&lt;slug&gt;:achievement:&lt;key&gt;".</summary>
        [JsonPropertyName("achievement")]
        public string Achievement { get; set; } = "";

        [JsonPropertyName("issued_at")]
        public DateTimeOffset IssuedAt { get; set; }

        [JsonPropertyName("authenticity")]
        public Authenticity Authenticity { get; set; } = new Authenticity();

        [JsonPropertyName("validity")]
        public Validity Validity { get; set; } = new Validity();

        [JsonPropertyName("history")]
        public List<AttestationHistoryEntry> History { get; set; } = new List<AttestationHistoryEntry>();
    }

    // ListMyAchievementsResponse stays hand-written, keeping List<VerifiedAttestation>: the
    // generated Avalon.Sdk.Generated.ListMyAchievementsResponse's Achievements field is typed
    // as ICollection<AttestationReadResponse>, and AttestationReadResponse is one of the
    // deliberately-excluded oneOf schemas (its nested AuthenticityResponse/ValidityResponse
    // generate as empty stub classes) — migrating this DTO would silently drop every
    // authenticity/validity field this file exists to carry.
    internal sealed class ListMyAchievementsResponse
    {
        [JsonPropertyName("achievements")]
        public List<VerifiedAttestation> Achievements { get; set; } = new List<VerifiedAttestation>();

        [JsonPropertyName("next_cursor")]
        public Guid? NextCursor { get; set; }
    }

    public sealed partial class Session
    {
        /// <summary>Signs <paramref name="message"/> with this session's configured
        /// <see cref="SigningKey"/> (a 32-byte Ed25519 seed) — pure-managed Ed25519 via
        /// BouncyCastle, no native dependency, so this stays Unity/IL2CPP-safe.</summary>
        private byte[] SignWithIssuerKey(byte[] message)
        {
            var privateKey = new Ed25519PrivateKeyParameters(SigningKey, 0);
            var signer = new Ed25519Signer();
            signer.Init(true, privateKey);
            signer.BlockUpdate(message, 0, message.Length);
            return signer.GenerateSignature();
        }

        /// <summary>The exact bytes this integrator's key signs to authorize an attestation.
        /// <paramref name="claimKind"/> is <c>"achievement"</c> for game issuers and
        /// <c>"milestone"</c> for app/service issuers, folded into the signed bytes so a
        /// signature minted under one claim vocabulary can never be replayed as the other.
        /// This SDK builds these bytes itself rather than sharing source with the server;
        /// <c>conformance/vectors/attestation-signing.json</c> is what keeps the two in
        /// step.</summary>
        private static byte[] AttestationSigningBytes(
            string claimKind, string issuerRef, Guid subject, string achievement) =>
            Encoding.UTF8.GetBytes($"avalon:{claimKind}.issued:v1:{issuerRef}:{subject}:{achievement}");

        /// <summary>The exact bytes this integrator's key signs to authorize a bulk issuance:
        /// one signature over the whole ordered list, each ref length-prefixed
        /// big-endian so two different orderings or splits of the same refs can never sign
        /// identically. Checked against the same shared vectors as
        /// <see cref="AttestationSigningBytes"/>.</summary>
        private static byte[] BulkAttestationSigningBytes(
            string claimKind, string issuerRef, Guid subject, IReadOnlyList<string> achievementRefs)
        {
            var message = new List<byte>(
                Encoding.UTF8.GetBytes($"avalon:{claimKind}.issued.bulk:v1:{issuerRef}:{subject}:"));
            message.AddRange(BitConverter.GetBytes((uint)achievementRefs.Count).Reverse());
            foreach (var achievementRef in achievementRefs)
            {
                var bytes = Encoding.UTF8.GetBytes(achievementRef);
                message.AddRange(BitConverter.GetBytes((uint)bytes.Length).Reverse());
                message.AddRange(bytes);
            }
            return message.ToArray();
        }

        /// <summary>The exact bytes this integrator's key signs to authorize a revocation.
        /// Checked against the same shared vectors as
        /// <see cref="AttestationSigningBytes"/>.</summary>
        private static byte[] RevocationSigningBytes(
            string claimKind, string issuerRef, Guid attestationId, string reasonCode) =>
            Encoding.UTF8.GetBytes($"avalon:{claimKind}.revoked:v1:{issuerRef}:{attestationId}:{reasonCode}");

        /// <summary>POST /integrations/{slug}/challenge — an ephemeral, single-use
        /// challenge proving this integrator's key is making this HTTP call right now.</summary>
        private async Task<Avalon.Sdk.Generated.IntegratorChallengeResponse> RequestChallengeAsync(string slug, CancellationToken ct)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{slug}/challenge");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.IntegratorChallengeResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>GET /me/achievements — requires achievements.read. This identity's own
        /// attestation history; recognition is left to the caller's own policy (see this file's
        /// header comment). Mirrors the Rust SDK's <c>Session::achievements()</c>.</summary>
        public async Task<IReadOnlyList<VerifiedAttestation>> GetAchievementsAsync(CancellationToken ct = default)
        {
            Require("achievements.read");

            // limit=200 — the server's own max page size; a caller with more than 200
            // attestations needs a paginated entry point this SDK doesn't expose yet, same
            // documented gap as the Rust SDK's own `fetch_achievements`.
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/me/achievements?limit=200");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", Token);
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var page = await ReadJsonAsync<ListMyAchievementsResponse>(response, ct).ConfigureAwait(false);
            return page.Achievements;
        }

        /// <summary>Issues <paramref name="key"/> (an achievement defined by this integrator) to
        /// this session's own identity, signed locally with <see cref="SigningKey"/>. Returns the
        /// new attestation's id. Throws <see cref="MissingIssuerCredentialsException"/>, without
        /// making any HTTP call, if <see cref="IntegratorSlug"/>/<see cref="SigningKey"/> weren't
        /// configured. Mirrors the Rust SDK's <c>Session::issue_achievement()</c>.</summary>
        public async Task<Guid> IssueAchievementAsync(string key, CancellationToken ct = default)
        {
            Require("achievements.issue");

            if (IntegratorSlug is null || SigningKey is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            if (!Guid.TryParse(IntegratorKeyId, out var keyId))
            {
                throw new MissingIssuerCredentialsException();
            }

            var challenge = await RequestChallengeAsync(IntegratorSlug, ct).ConfigureAwait(false);
            var nonce = Convert.FromBase64String(challenge.Nonce);
            var challengeSignature = SignWithIssuerKey(nonce);

            var issuerRef = $"game:{IntegratorSlug}";
            var achievement = $"game:{IntegratorSlug}:achievement:{key}";
            var signingBytes = AttestationSigningBytes("achievement", issuerRef, IdentityGuid, achievement);
            var signature = SignWithIssuerKey(signingBytes);

            using var request = new HttpRequestMessage(
                HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/achievements/{key}/issue");
            request.Headers.Add("x-avalon-integrator-key-id", IntegratorKeyId);
            request.Headers.Add("x-avalon-integrator-challenge-id", challenge.ChallengeId.ToString());
            request.Headers.Add("x-avalon-integrator-signature", Convert.ToBase64String(challengeSignature));
            request.Headers.Add("x-avalon-identity-id", IdentityGuid.ToString());
            // A fresh Idempotency-Key per call: a caller that retries this whole
            // call after a dropped response mints a new key, same as the Rust SDK's
            // `submit_achievement_issuance` — this method doesn't itself retry.
            request.Headers.Add("idempotency-key", Guid.NewGuid().ToString());
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.IssueAttestationRequest
                {
                    KeyId = keyId,
                    Signature = Convert.ToBase64String(signature),
                }),
                Encoding.UTF8, "application/json");

            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var body = await ReadJsonAsync<Avalon.Sdk.Generated.AttestationResponse>(response, ct).ConfigureAwait(false);
            return body.Id;
        }

        /// <summary>GET /attestations/{id} — public, unauthenticated. Reuses
        /// <see cref="VerifiedAttestation"/> as the return shape: its fields are a
        /// strict subset of the wire response (which also carries a <c>proof</c> this SDK has no
        /// use for outside issuance), so the same hand-written type this file already needs for
        /// <see cref="GetAchievementsAsync"/> — deliberately not
        /// <c>Avalon.Sdk.Generated.AttestationReadResponse</c>, whose nested
        /// <c>AuthenticityResponse</c>/<c>ValidityResponse</c> are excluded <c>oneOf</c> stubs —
        /// covers this read too.</summary>
        public async Task<VerifiedAttestation> GetAttestationAsync(Guid attestationId, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/attestations/{attestationId}");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<VerifiedAttestation>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /attestations/{id}/revoke — only the attestation's original issuer may
        /// revoke it. Authenticated the same challenge-response way every
        /// issuer-credentialed write in this file is; not gated behind a user capability grant,
        /// since this is the issuer asserting something about its own issuance, not acting on a
        /// specific player's behalf. The embedded signature covers
        /// <c>avalon_protocol::achievements::revocation_signing_bytes</c> for
        /// <paramref name="claimKind"/> ("achievement" or "milestone" — must match whichever
        /// vocabulary originally issued this attestation).</summary>
        public async Task<Avalon.Sdk.Generated.RevocationResponse> RevokeAttestationAsync(
            Guid attestationId, string claimKind, string issuerRef, string reasonCode, string reason, CancellationToken ct = default)
        {
            if (IntegratorSlug is null || SigningKey is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            if (!Guid.TryParse(IntegratorKeyId, out var keyId))
            {
                throw new MissingIssuerCredentialsException();
            }

            var signingBytes = RevocationSigningBytes(claimKind, issuerRef, attestationId, reasonCode);
            var signature = SignWithIssuerKey(signingBytes);

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/attestations/{attestationId}/revoke");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.RevokeAttestationRequest
            {
                KeyId = keyId,
                Signature = Convert.ToBase64String(signature),
                ReasonCode = reasonCode,
                Reason = reason,
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.RevocationResponse>(response, ct).ConfigureAwait(false);
        }

        // --- Achievement/milestone definition CRUD ---
        //
        // Create/update/list, for both claim vocabularies. Milestones share the
        // exact same request/response wire shapes as achievements
        // (crates/server/src/achievements.rs's own create_definition/
        // list_achievement_definitions/list_milestone_definitions are one shared
        // implementation under two routes) — only the path and, for the two
        // signature-bearing issuance methods below, the id namespace differ.

        /// <summary>POST /integrations/{slug}/achievements — defines a new achievement this
        /// integrator can later issue. Challenge-authenticated only, no user capability (the
        /// integrator declaring its own vocabulary, not acting on a player's behalf).</summary>
        public async Task<Avalon.Sdk.Generated.AchievementDefinitionResponse> CreateAchievementDefinitionAsync(
            string key, string name, string description, string? schema = null, string? icon = null, string? iconUrl = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/achievements");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.CreateAchievementDefinitionRequest
            {
                Key = key,
                Name = name,
                Description = description,
                Schema = schema,
                Icon = icon,
                IconUrl = iconUrl,
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.AchievementDefinitionResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>POST /integrations/{slug}/milestones — the App/Service-category
        /// equivalent of <see cref="CreateAchievementDefinitionAsync"/>;
        /// rejected server-side with a claim-vocabulary mismatch if this integrator is
        /// registered as a Game.</summary>
        public async Task<Avalon.Sdk.Generated.AchievementDefinitionResponse> CreateMilestoneDefinitionAsync(
            string key, string name, string description, string? schema = null, string? icon = null, string? iconUrl = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/milestones");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.CreateAchievementDefinitionRequest
            {
                Key = key,
                Name = name,
                Description = description,
                Schema = schema,
                Icon = icon,
                IconUrl = iconUrl,
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.AchievementDefinitionResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>PATCH /integrations/{slug}/achievements/{key} — every field is
        /// "absent means untouched", including <paramref name="retired"/> (one-way: never used
        /// to un-retire, matching the server's own doc comment).</summary>
        public async Task<Avalon.Sdk.Generated.AchievementDefinitionResponse> UpdateAchievementDefinitionAsync(
            string key, string? name = null, string? description = null, string? schema = null,
            string? icon = null, string? iconUrl = null, bool? retired = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Patch, $"{ServerUrl}/integrations/{IntegratorSlug}/achievements/{key}");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.UpdateAchievementDefinitionRequest
            {
                Name = name,
                Description = description,
                Schema = schema,
                Icon = icon,
                IconUrl = iconUrl,
                Retired = retired,
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.AchievementDefinitionResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>PATCH /integrations/{slug}/milestones/{key} — the App/Service-category
        /// equivalent of <see cref="UpdateAchievementDefinitionAsync"/>.</summary>
        public async Task<Avalon.Sdk.Generated.AchievementDefinitionResponse> UpdateMilestoneDefinitionAsync(
            string key, string? name = null, string? description = null, string? schema = null,
            string? icon = null, string? iconUrl = null, bool? retired = null, CancellationToken ct = default)
        {
            if (IntegratorSlug is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            using var request = new HttpRequestMessage(HttpMethod.Patch, $"{ServerUrl}/integrations/{IntegratorSlug}/milestones/{key}");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Content = JsonContent(new Avalon.Sdk.Generated.UpdateAchievementDefinitionRequest
            {
                Name = name,
                Description = description,
                Schema = schema,
                Icon = icon,
                IconUrl = iconUrl,
                Retired = retired,
            });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<Avalon.Sdk.Generated.AchievementDefinitionResponse>(response, ct).ConfigureAwait(false);
        }

        /// <summary>GET /integrations/{slug}/achievements — every achievement this integrator
        /// has defined, retired ones included. Public, unauthenticated.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.AchievementDefinitionResponse>> ListAchievementDefinitionsAsync(
            string slug, CancellationToken ct = default) =>
            await GetJsonAsync<List<Avalon.Sdk.Generated.AchievementDefinitionResponse>>(
                $"{ServerUrl}/integrations/{slug}/achievements", ct).ConfigureAwait(false)
            ?? new List<Avalon.Sdk.Generated.AchievementDefinitionResponse>();

        /// <summary>GET /integrations/{slug}/milestones — the App/Service-category equivalent
        /// of <see cref="ListAchievementDefinitionsAsync"/>. Public, unauthenticated.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.AchievementDefinitionResponse>> ListMilestoneDefinitionsAsync(
            string slug, CancellationToken ct = default) =>
            await GetJsonAsync<List<Avalon.Sdk.Generated.AchievementDefinitionResponse>>(
                $"{ServerUrl}/integrations/{slug}/milestones", ct).ConfigureAwait(false)
            ?? new List<Avalon.Sdk.Generated.AchievementDefinitionResponse>();

        /// <summary>POST /integrations/{slug}/milestones/{key}/issue — the App/Service-category
        /// equivalent of <see cref="IssueAchievementAsync"/>.
        /// <paramref name="category"/> is <c>"app"</c> or <c>"service"</c> — whichever this
        /// integrator actually registered as — since the signed achievement-ref namespace
        /// (<c>&lt;category&gt;:&lt;slug&gt;:milestone:&lt;key&gt;</c>) depends on it and this
        /// SDK has no other way to know a caller's own registered category.</summary>
        public async Task<Guid> IssueMilestoneAsync(string key, string category, CancellationToken ct = default)
        {
            Require("milestones.issue");

            if (IntegratorSlug is null || SigningKey is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            if (!Guid.TryParse(IntegratorKeyId, out var keyId))
            {
                throw new MissingIssuerCredentialsException();
            }

            var challenge = await RequestChallengeAsync(IntegratorSlug, ct).ConfigureAwait(false);
            var nonce = Convert.FromBase64String(challenge.Nonce);
            var challengeSignature = SignWithIssuerKey(nonce);

            var issuerRef = $"{category}:{IntegratorSlug}";
            var achievement = $"{category}:{IntegratorSlug}:milestone:{key}";
            var signingBytes = AttestationSigningBytes("milestone", issuerRef, IdentityGuid, achievement);
            var signature = SignWithIssuerKey(signingBytes);

            using var request = new HttpRequestMessage(
                HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/milestones/{key}/issue");
            request.Headers.Add("x-avalon-integrator-key-id", IntegratorKeyId);
            request.Headers.Add("x-avalon-integrator-challenge-id", challenge.ChallengeId.ToString());
            request.Headers.Add("x-avalon-integrator-signature", Convert.ToBase64String(challengeSignature));
            request.Headers.Add("x-avalon-identity-id", IdentityGuid.ToString());
            request.Headers.Add("idempotency-key", Guid.NewGuid().ToString());
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.IssueAttestationRequest
                {
                    KeyId = keyId,
                    Signature = Convert.ToBase64String(signature),
                }),
                Encoding.UTF8, "application/json");

            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var body = await ReadJsonAsync<Avalon.Sdk.Generated.AttestationResponse>(response, ct).ConfigureAwait(false);
            return body.Id;
        }

        // --- Bulk issuance ---
        //
        // One challenge-response proof plus one signature over the whole ordered
        // claim list (avalon_protocol::achievements::bulk_attestation_signing_bytes)
        // — never a per-claim signature. BulkIssueAttestationResponse's own
        // `results` are hand-rolled here (BulkClaimResult) rather than the
        // generated type, for the same reason ListMyAchievementsResponse is: the
        // generated response's own oneOf member is one of this codegen's
        // deliberately-excluded stubs.

        /// <summary>One claim's own outcome from a bulk issuance call — mirrors the server's
        /// own tagged <c>BulkClaimResult</c> (<c>"issued"</c>/<c>"failed"</c>).</summary>
        public sealed class BulkClaimOutcome
        {
            [JsonPropertyName("status")]
            public string Status { get; set; } = "";

            [JsonPropertyName("key")]
            public string Key { get; set; } = "";

            /// <summary>Present only when <see cref="Status"/> is <c>"issued"</c>.</summary>
            [JsonPropertyName("attestation")]
            public Avalon.Sdk.Generated.AttestationResponse? Attestation { get; set; }

            /// <summary>Present only when <see cref="Status"/> is <c>"failed"</c> — a stable
            /// machine-readable code, matching this repo's own <c>AppError::code</c>
            /// convention.</summary>
            [JsonPropertyName("code")]
            public string? Code { get; set; }

            /// <summary>Present only when <see cref="Status"/> is <c>"failed"</c>.</summary>
            [JsonPropertyName("error")]
            public string? Error { get; set; }

            public bool IsIssued => Status == "issued";
        }

        internal sealed class BulkIssueResultsResponse
        {
            [JsonPropertyName("results")]
            public List<BulkClaimOutcome> Results { get; set; } = new List<BulkClaimOutcome>();
        }

        /// <summary>One claim to submit in a bulk issuance call — mirrors the server's own
        /// <c>BulkClaimRequest</c>.</summary>
        public sealed class BulkClaim
        {
            public BulkClaim(string key, object? evidence = null)
            {
                Key = key;
                Evidence = evidence;
            }

            public string Key { get; }
            public object? Evidence { get; }
        }

        /// <summary>Everything <see cref="BulkIssueAchievementsAsync"/>/
        /// <see cref="BulkIssueMilestonesAsync"/> need to actually send their own literal-route
        /// request — split out only so each keeps its own literal
        /// <c>new HttpRequestMessage(HttpMethod.Post, $"...")</c> call site (matching this
        /// SDK's, and `scripts/check-sdk-coverage.py`'s, existing "literal path per route"
        /// convention) while sharing the signing-bytes/body-building logic.
        /// <paramref name="claimKind"/> is folded into the domain-tagged signing bytes
        /// (<c>avalon:&lt;claim_kind&gt;.issued.bulk:v1:...</c>) exactly as
        /// <c>bulk_attestation_signing_bytes</c> requires, with each achievement ref
        /// length-prefixed so two different orderings (or a key containing delimiter-like
        /// bytes) can never sign identically.</summary>
        private async Task<HttpContent> PrepareBulkIssueContentAsync(
            string claimKind, string issuerNamespace, IReadOnlyList<BulkClaim> claims, CancellationToken ct)
        {
            if (IntegratorSlug is null || SigningKey is null)
            {
                throw new MissingIssuerCredentialsException();
            }
            if (!Guid.TryParse(IntegratorKeyId, out var keyId))
            {
                throw new MissingIssuerCredentialsException();
            }

            var issuerRef = $"{issuerNamespace}:{IntegratorSlug}";
            var achievementRefs = new List<string>(claims.Count);
            foreach (var claim in claims)
            {
                achievementRefs.Add($"{issuerNamespace}:{IntegratorSlug}:{claimKind}:{claim.Key}");
            }

            var signingBytes = BulkAttestationSigningBytes(claimKind, issuerRef, IdentityGuid, achievementRefs);
            var signature = SignWithIssuerKey(signingBytes);

            return new StringContent(
                JsonSerializer.Serialize(new
                {
                    key_id = keyId,
                    signature = Convert.ToBase64String(signature),
                    claims = claims.Select(c => new { key = c.Key, evidence = c.Evidence }).ToList(),
                }),
                Encoding.UTF8, "application/json");
        }

        /// <summary>POST /integrations/{slug}/achievements/bulk-issue —
        /// requires achievements.issue, same capability a single issuance does. Never
        /// all-or-nothing: one claim failing (an unknown/retired key) doesn't fail the rest —
        /// see each result's own <see cref="BulkClaimOutcome.Status"/>.</summary>
        public async Task<IReadOnlyList<BulkClaimOutcome>> BulkIssueAchievementsAsync(
            IReadOnlyList<BulkClaim> claims, CancellationToken ct = default)
        {
            Require("achievements.issue");
            var content = await PrepareBulkIssueContentAsync("achievement", "game", claims, ct).ConfigureAwait(false);

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/achievements/bulk-issue");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Headers.Add("x-avalon-identity-id", IdentityGuid.ToString());
            request.Headers.Add("idempotency-key", Guid.NewGuid().ToString());
            request.Content = content;

            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var body = await ReadJsonAsync<BulkIssueResultsResponse>(response, ct).ConfigureAwait(false);
            return body.Results;
        }

        /// <summary>POST /integrations/{slug}/milestones/bulk-issue — the App/Service-category
        /// equivalent of <see cref="BulkIssueAchievementsAsync"/>. <paramref name="category"/>
        /// is <c>"app"</c> or <c>"service"</c>, same reasoning as
        /// <see cref="IssueMilestoneAsync"/>.</summary>
        public async Task<IReadOnlyList<BulkClaimOutcome>> BulkIssueMilestonesAsync(
            string category, IReadOnlyList<BulkClaim> claims, CancellationToken ct = default)
        {
            Require("milestones.issue");
            var content = await PrepareBulkIssueContentAsync("milestone", category, claims, ct).ConfigureAwait(false);

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/milestones/bulk-issue");
            await AttachIntegratorAuthAsync(request, ct).ConfigureAwait(false);
            request.Headers.Add("x-avalon-identity-id", IdentityGuid.ToString());
            request.Headers.Add("idempotency-key", Guid.NewGuid().ToString());
            request.Content = content;

            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var body = await ReadJsonAsync<BulkIssueResultsResponse>(response, ct).ConfigureAwait(false);
            return body.Results;
        }
    }
}
