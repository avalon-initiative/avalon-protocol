// Multi-passkey management (issue #200) on AccountSession — WebAuthn login credentials,
// distinct from AccountSession.Devices.cs (event-signing keys). Mirrors
// crates/sdk/src/account/passkeys.rs.
//
// AddPasskeyAsync (registering an *additional* passkey) is deliberately not ported here —
// it drives a real WebAuthn registration ceremony, same as Register/AccountLogin at the
// AvalonClient level (see AccountSession.cs's own header comment for the scoping call).
// ListPasskeysAsync/RenamePasskeyAsync/RevokePasskeyAsync need no ceremony and are fully
// supported.

using System;
using System.Collections.Generic;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>One of this identity's registered WebAuthn passkeys. Mirrors the Rust
    /// SDK's <c>account::passkeys::Passkey</c>.</summary>
    public sealed class AccountPasskey
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("label")]
        public string? Label { get; set; }

        [JsonPropertyName("added_at")]
        public DateTimeOffset AddedAt { get; set; }
    }

    public sealed partial class AccountSession
    {
        /// <summary><c>GET /me/passkeys</c> — every passkey registered to this identity.</summary>
        public async Task<IReadOnlyList<AccountPasskey>> ListPasskeysAsync(CancellationToken ct = default) =>
            await GetAsync<List<AccountPasskey>>("/me/passkeys", ct).ConfigureAwait(false);

        /// <summary><c>PATCH /me/passkeys/{id}</c> — relabels a passkey. Not
        /// signature-required.</summary>
        public async Task<AccountPasskey> RenamePasskeyAsync(Guid passkeyId, string label, CancellationToken ct = default) =>
            await PatchAsync<Avalon.Sdk.Generated.RenamePasskeyRequest, AccountPasskey>(
                $"/me/passkeys/{passkeyId}", new Avalon.Sdk.Generated.RenamePasskeyRequest { Label = label }, ct).ConfigureAwait(false);

        /// <summary><c>POST /me/passkeys/{id}/revoke</c> — always signs
        /// (<c>passkey.revoke_last</c>, <c>[passkey_id, identity_id]</c>), whether or not
        /// this is actually the identity's last remaining passkey: #697/#698 only require a
        /// fresh signature in the last-passkey case, but an unused-but-valid signature on a
        /// non-last revoke is harmless, same simplification the Hub frontend and Rust SDK
        /// already make.</summary>
        public async Task RevokePasskeyAsync(Guid passkeyId, CancellationToken ct = default)
        {
            var (signingKeyId, signature) = Sign("passkey.revoke_last", passkeyId.ToString(), IdentityGuid.ToString());
            await PostNoResponseAsync(
                $"/me/passkeys/{passkeyId}/revoke",
                new Avalon.Sdk.Generated.RevokePasskeyRequest { SigningKeyId = signingKeyId, Signature = signature },
                ct).ConfigureAwait(false);
        }
    }
}
