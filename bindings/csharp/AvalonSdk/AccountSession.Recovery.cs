// Social recovery (issue #201/#443) on AccountSession — M-of-N guardian-based recovery when
// every passkey is lost. Mirrors crates/sdk/src/account/recovery.rs. No WebAuthn ceremony
// involved (the unauthenticated recovery-initiation calls themselves are a deliberate,
// separate gap noted in docs/architecture/sdk.md, same as the Rust SDK's own #699 scope note
// — they don't belong on AccountSession at all, since the whole premise is the caller has no
// session for the identity being recovered).

using System;
using System.Collections.Generic;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>The caller's own guardian configuration. Mirrors the Rust SDK's
    /// <c>account::recovery::GuardianSettings</c>.</summary>
    public sealed class AccountGuardianSettings
    {
        [JsonPropertyName("guardian_ids")]
        public List<Guid> GuardianIds { get; set; } = new List<Guid>();

        [JsonPropertyName("threshold")]
        public int Threshold { get; set; }

        [JsonPropertyName("updated_at")]
        public DateTimeOffset? UpdatedAt { get; set; }
    }

    /// <summary>A recovery attempt in progress or resolved. Mirrors the Rust SDK's
    /// <c>account::recovery::RecoveryRequest</c>.</summary>
    public sealed class AccountRecoveryRequest
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        /// <summary>"pending", "approved", "completed", "cancelled", or "expired" —
        /// <c>crates/server/src/recovery.rs</c>'s own stable vocabulary.</summary>
        [JsonPropertyName("status")]
        public string Status { get; set; } = "";

        [JsonPropertyName("threshold")]
        public int Threshold { get; set; }

        [JsonPropertyName("approvals_count")]
        public long ApprovalsCount { get; set; }

        [JsonPropertyName("requested_at")]
        public DateTimeOffset RequestedAt { get; set; }

        [JsonPropertyName("delay_ends_at")]
        public DateTimeOffset? DelayEndsAt { get; set; }
    }

    /// <summary>One active recovery request where the caller is currently a guardian.
    /// Mirrors the Rust SDK's <c>account::recovery::GuardianRequest</c>.</summary>
    public sealed class AccountGuardianRequest
    {
        [JsonPropertyName("request")]
        public AccountRecoveryRequest Request { get; set; } = new AccountRecoveryRequest();

        [JsonPropertyName("already_approved")]
        public bool AlreadyApproved { get; set; }
    }

    /// <summary>An identity currently relying on the caller as a recovery guardian. Mirrors
    /// the Rust SDK's <c>account::recovery::GuardianOf</c>.</summary>
    public sealed class AccountGuardianOf
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        [JsonPropertyName("display_name")]
        public string DisplayName { get; set; } = "";

        [JsonPropertyName("added_at")]
        public DateTimeOffset AddedAt { get; set; }
    }

    public sealed partial class AccountSession
    {
        /// <summary><c>GET /me/recovery/guardians</c>.</summary>
        public async Task<AccountGuardianSettings> GuardiansAsync(CancellationToken ct = default) =>
            await GetAsync<AccountGuardianSettings>("/me/recovery/guardians", ct).ConfigureAwait(false);

        /// <summary><c>PUT /me/recovery/guardians</c> — always signs
        /// (<c>recovery.guardians.set</c>, <c>[identity_id, sorted guardian ids
        /// comma-joined, threshold]</c>), whether or not this call actually removes a
        /// guardian or raises the threshold (the only cases #697 requires it for) — same
        /// unused-but-valid-signature-is-harmless simplification every other
        /// conditionally-signed method makes.</summary>
        public async Task<AccountGuardianSettings> SetGuardiansAsync(IReadOnlyList<Guid> guardianIds, int threshold, CancellationToken ct = default)
        {
            var sorted = new List<string>();
            foreach (var id in guardianIds)
            {
                sorted.Add(id.ToString());
            }
            sorted.Sort(StringComparer.Ordinal);
            var (signingKeyId, signature) = Sign(
                "recovery.guardians.set", IdentityGuid.ToString(), string.Join(",", sorted), threshold.ToString());
            return await PutAsync<Avalon.Sdk.Generated.SetGuardiansRequest, AccountGuardianSettings>(
                "/me/recovery/guardians",
                new Avalon.Sdk.Generated.SetGuardiansRequest
                {
                    GuardianIds = new List<Guid>(guardianIds),
                    Threshold = threshold,
                    SigningKeyId = signingKeyId,
                    Signature = signature,
                },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>GET /me/recovery/status</c> — the caller's own in-flight recovery
        /// request, if any.</summary>
        public async Task<AccountRecoveryRequest?> MyRecoveryStatusAsync(CancellationToken ct = default) =>
            await GetNullableAsync<AccountRecoveryRequest>("/me/recovery/status", ct).ConfigureAwait(false);

        /// <summary><c>GET /me/recovery/guardian-requests</c> — every active request where
        /// the caller is currently a guardian.</summary>
        public async Task<IReadOnlyList<AccountGuardianRequest>> GuardianRequestsAsync(CancellationToken ct = default) =>
            await GetAsync<List<AccountGuardianRequest>>("/me/recovery/guardian-requests", ct).ConfigureAwait(false);

        /// <summary><c>GET /me/recovery/guardian-of</c> — every identity currently relying
        /// on the caller as a guardian.</summary>
        public async Task<IReadOnlyList<AccountGuardianOf>> GuardianOfAsync(CancellationToken ct = default) =>
            await GetAsync<List<AccountGuardianOf>>("/me/recovery/guardian-of", ct).ConfigureAwait(false);

        /// <summary><c>DELETE /me/recovery/guardian-of/{identity_id}</c> — the caller
        /// self-removing as someone else's guardian. Not signature-required (self-removal
        /// only narrows a guardian assignment).</summary>
        public async Task ResignAsGuardianAsync(Guid identityId, CancellationToken ct = default) =>
            await DeleteAsync($"/me/recovery/guardian-of/{identityId}", ct).ConfigureAwait(false);

        /// <summary><c>POST /recovery/requests/{id}/approve</c> — a guardian approving
        /// someone else's in-flight recovery request.</summary>
        public async Task<AccountRecoveryRequest> ApproveRecoveryRequestAsync(Guid requestId, CancellationToken ct = default) =>
            await PostEmptyAsync<AccountRecoveryRequest>($"/recovery/requests/{requestId}/approve", ct).ConfigureAwait(false);

        /// <summary><c>POST /recovery/requests/{id}/cancel</c> — the veto path: either the
        /// identity's own owner or any current guardian.</summary>
        public async Task<AccountRecoveryRequest> CancelRecoveryRequestAsync(Guid requestId, string? reason = null, CancellationToken ct = default) =>
            await PostAsync<Avalon.Sdk.Generated.CancelRecoveryRequest, AccountRecoveryRequest>(
                $"/recovery/requests/{requestId}/cancel", new Avalon.Sdk.Generated.CancelRecoveryRequest { Reason = reason }, ct).ConfigureAwait(false);
    }
}
