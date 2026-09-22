// Achievements (issue #34, wired here for #51) — read a user's own attestation
// history and issue a new attestation on this integrator's own behalf.
// Mirrors crates/sdk/src/achievements.rs's single-claim (non-bulk,
// non-revocation) surface; bulk issuance (#495) and revocation (#498) are
// separate, later Rust-side tickets with no C# equivalent yet — #51's own
// invariant ("never exposes a method the Rust SDK doesn't have, and vice
// versa") is about keeping the two surfaces from silently diverging, not
// about building every Rust method on day one.
//
// Reading (GetAchievementsAsync) uses the identity's own bearer token
// against GET /me/achievements — authenticity/validity come back as the
// server computed them; recognition is deliberately never present (ADR #76:
// authenticity, validity, and recognition are three separate questions —
// this SDK never collapses them into one boolean).
//
// Issuing (IssueAchievementAsync) needs this integrator's own signing key —
// the server never sees it, only a detached signature. Two independent
// proofs go out: an ephemeral challenge-response proving this key is making
// the HTTP call right now, and a separate signature embedded in the request
// body over the attestation's own canonical bytes.

using System;
using System.Collections.Generic;
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
    /// header comment (ADR #76). Mirrors <c>avalon_sdk::achievements::VerifiedAttestation</c>.
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

        /// <summary>The exact bytes this integrator's key signs to authorize an attestation —
        /// must match <c>avalon_protocol::achievements::attestation_signing_bytes</c> exactly.
        /// This SDK defines its own copy rather than depending on the server's private
        /// construction, same posture as every other signed request in this repo.</summary>
        private static byte[] AttestationSigningBytes(string issuerRef, Guid subject, string achievement) =>
            Encoding.UTF8.GetBytes($"avalon:achievement.issued:v1:{issuerRef}:{subject}:{achievement}");

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
            var signingBytes = AttestationSigningBytes(issuerRef, IdentityGuid, achievement);
            var signature = SignWithIssuerKey(signingBytes);

            using var request = new HttpRequestMessage(
                HttpMethod.Post, $"{ServerUrl}/integrations/{IntegratorSlug}/achievements/{key}/issue");
            request.Headers.Add("x-avalon-integrator-key-id", IntegratorKeyId);
            request.Headers.Add("x-avalon-integrator-challenge-id", challenge.ChallengeId.ToString());
            request.Headers.Add("x-avalon-integrator-signature", Convert.ToBase64String(challengeSignature));
            request.Headers.Add("x-avalon-identity-id", IdentityGuid.ToString());
            // A fresh Idempotency-Key per call (issue #47): a caller that retries this whole
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
    }
}
