// Unit tests for AccountSession — the canonical-message shape byte-for-byte
// against the known avalon:<action_tag>:v1:... format, plus representative signed-call round
// trips through StubHttpMessageHandler proving the signature header/body actually gets
// attached correctly (or, with no local signing key, that it's sent as explicit JSON nulls
// rather than omitted — the server's own NO_REGISTERED_SIGNING_KEY/FRESH_SIGNATURE_REQUIRED
// split needs to see the fields, not a missing body).

using System;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Avalon.Sdk;
using Org.BouncyCastle.Crypto.Parameters;
using Org.BouncyCastle.Crypto.Signers;
using Org.BouncyCastle.Security;
using Xunit;

namespace Avalon.Sdk.Tests;

public class AccountSessionTests
{
    [Fact]
    public void CanonicalMessage_MatchesTheServerShape()
    {
        var message = AccountSession.CanonicalMessage("guild.transfer_ownership", "g1", "from1", "to1");

        Assert.Equal("avalon:guild.transfer_ownership:v1:g1:from1:to1", Encoding.UTF8.GetString(message));
    }

    [Fact]
    public void CanonicalMessage_WithNoFields_IsJustTheTag()
    {
        var message = AccountSession.CanonicalMessage("integration.connect");

        Assert.Equal("avalon:integration.connect:v1", Encoding.UTF8.GetString(message));
    }

    [Fact]
    public async Task RevokePasskeyAsync_WithNoLocalSigningKey_SendsExplicitNullSignatureFields()
    {
        var handler = new StubHttpMessageHandler().Enqueue(System.Net.HttpStatusCode.OK, "");
        var session = AccountSession.ForTesting(handler.ToHttpClient());
        var passkeyId = Guid.NewGuid();

        await session.RevokePasskeyAsync(passkeyId);

        var request = Assert.Single(handler.Requests);
        Assert.Equal(HttpMethod.Post, request.Method);
        Assert.EndsWith($"/me/passkeys/{passkeyId}/revoke", request.Url);
        using var body = JsonDocument.Parse(request.Body!);
        Assert.Equal(JsonValueKind.Null, body.RootElement.GetProperty("signing_key_id").ValueKind);
        Assert.Equal(JsonValueKind.Null, body.RootElement.GetProperty("signature").ValueKind);
    }

    [Fact]
    public async Task ConnectIntegratorAsync_WithALocalSigningKey_AttachesAVerifiableSignature()
    {
        var privateKey = new Ed25519PrivateKeyParameters(new SecureRandom());
        var publicKey = privateKey.GeneratePublicKey();
        var signingKeyId = Guid.NewGuid();
        var signing = new AccountSession.SigningKeyMaterial(privateKey, signingKeyId);

        var handler = new StubHttpMessageHandler()
            .Enqueue(HttpStatusCodeOk(), @"{""binding_id"":""" + Guid.NewGuid() + @""",""integrator_id"":""" + Guid.NewGuid() + @""",""established_at"":""2026-01-01T00:00:00Z"",""granted_capabilities"":[""achievements.read""]}");
        var session = AccountSession.ForTesting(handler.ToHttpClient(), signing: signing);

        await session.ConnectIntegratorAsync("dragon-game", new[] { "achievements.read" });

        var request = Assert.Single(handler.Requests);
        using var body = JsonDocument.Parse(request.Body!);
        Assert.Equal(signingKeyId, body.RootElement.GetProperty("signing_key_id").GetGuid());
        var signatureBytes = Convert.FromBase64String(body.RootElement.GetProperty("signature").GetString()!);

        var expectedMessage = AccountSession.CanonicalMessage("integration.connect", "dragon-game", "achievements.read");
        var verifier = new Ed25519Signer();
        verifier.Init(false, publicKey);
        verifier.BlockUpdate(expectedMessage, 0, expectedMessage.Length);
        Assert.True(verifier.VerifySignature(signatureBytes));
    }

    [Fact]
    public async Task CreateRoleAsync_SignsWithGuildRoleCreateTag()
    {
        var privateKey = new Ed25519PrivateKeyParameters(new SecureRandom());
        var publicKey = privateKey.GeneratePublicKey();
        var signingKeyId = Guid.NewGuid();
        var signing = new AccountSession.SigningKeyMaterial(privateKey, signingKeyId);
        var guildId = Guid.NewGuid();

        var handler = new StubHttpMessageHandler()
            .Enqueue(HttpStatusCodeOk(), @"{""name_index"":1,""name"":""Officer"",""permissions"":[""manage_members""],""description"":"""",""badge"":{}}");
        var session = AccountSession.ForTesting(handler.ToHttpClient(), signing: signing);

        await session.CreateRoleAsync(guildId, "Officer", new[] { "manage_members" }, "");

        var request = Assert.Single(handler.Requests);
        using var body = JsonDocument.Parse(request.Body!);
        var signatureBytes = Convert.FromBase64String(body.RootElement.GetProperty("signature").GetString()!);

        var expectedMessage = AccountSession.CanonicalMessage("guild.role.create", guildId.ToString(), "Officer", "manage_members");
        var verifier = new Ed25519Signer();
        verifier.Init(false, publicKey);
        verifier.BlockUpdate(expectedMessage, 0, expectedMessage.Length);
        Assert.True(verifier.VerifySignature(signatureBytes));
    }

    [Fact]
    public async Task UpdateProfileAsync_OmitsUntouchedFieldsAndClearsEmptyStringFields()
    {
        var self = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue(HttpStatusCodeOk(),
            $@"{{""identity_id"":""{self}"",""identity_created_at"":""2026-01-01T00:00:00Z"",""display_name"":""newname"",""avatar_url"":null,""bio"":"""",""favorite_genres"":[],""pronouns"":null,""banner_url"":null,""status"":null,""links"":[],""timezone"":null,""theme_color"":null,""location"":null,""main_guild"":null}}");
        var session = AccountSession.ForTesting(handler.ToHttpClient(), identityId: self);

        await session.UpdateProfileAsync(new AccountProfileUpdate { DisplayName = "newname", Bio = "" });

        var request = Assert.Single(handler.Requests);
        using var body = JsonDocument.Parse(request.Body!);
        Assert.Equal("newname", body.RootElement.GetProperty("display_name").GetString());
        Assert.Equal("", body.RootElement.GetProperty("bio").GetString());
        // avatar_url was never set on the update — must be omitted entirely, not sent as null
        // (PATCH /me's own partial-update convention: absent means "leave untouched").
        Assert.False(body.RootElement.TryGetProperty("avatar_url", out _));
        Assert.Equal("newname", session.Profile.DisplayName);
    }

    private static System.Net.HttpStatusCode HttpStatusCodeOk() => System.Net.HttpStatusCode.OK;
}
