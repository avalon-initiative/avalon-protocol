using System;
using System.Linq;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class IntegratorRegistrationTests
{
    private static readonly byte[] TestSigningKey = Enumerable.Range(0, 32).Select(i => (byte)i).ToArray();

    [Fact]
    public async Task RegisterIntegratorAsync_PostsToIntegrations_NoAuthHeader()
    {
        var integratorId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        {
            "id": "{{integratorId}}", "slug": "dragons-inc", "name": "Dragons Inc",
            "owner_name": "Ashen Realms", "registered_at": "2026-01-01T00:00:00Z",
            "status": "active", "category": "integrator", "requested_capabilities": [],
            "credential": { "integrator_id": "{{integratorId}}", "key_id": "11111111-1111-1111-1111-111111111111" }
        }
        """);
        var client = new AvalonClient(new AvalonConfig("http://test", "unused"), handler.ToHttpClient());

        var integrator = await client.RegisterIntegratorAsync(
            "dragons-inc", "Dragons Inc", "Ashen Realms", "ed25519", Convert.ToBase64String(new byte[32]));

        Assert.Equal("dragons-inc", integrator.Slug);
        Assert.Equal(HttpMethod.Post, handler.Requests[0].Method);
        Assert.Contains("/integrations", handler.Requests[0].Url);
        Assert.Null(handler.Requests[0].AuthorizationToken);
    }

    [Fact]
    public async Task RegisterIssuerAsync_DrivesTheChallengeRoundTrip()
    {
        var nonce = Convert.ToBase64String(new byte[] { 5, 6, 7, 8 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue("""{ "issuer_ref": "game:dragons-inc", "registered_at": "2026-01-01T00:00:00Z" }""");
        var client = new AvalonClient(new AvalonConfig("http://test", "unused"), handler.ToHttpClient());

        var registration = await client.RegisterIssuerAsync(TestSigningKey, "game:dragons-inc", "avalon-dev-local");

        Assert.Equal("game:dragons-inc", registration.IssuerRef);
        Assert.Equal(2, handler.Requests.Count);
        Assert.Contains("/issuers/registration-challenge", handler.Requests[0].Url);
        Assert.Contains("/issuers/register", handler.Requests[1].Url);
    }

    [Fact]
    public async Task IntegratorWhoamiAsync_WithoutConfiguredIssuerCredentials_ThrowsBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<MissingIssuerCredentialsException>(() => session.IntegratorWhoamiAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task IntegratorWhoamiAsync_SendsAChallengeThenReturnsTheIntegratorId()
    {
        var integratorId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""{ "integrator_id": "{{integratorId}}" }""");
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var id = await session.IntegratorWhoamiAsync();

        Assert.Equal(integratorId, id);
        Assert.Contains("/integrations/whoami", handler.Requests[1].Url);
    }

    [Fact]
    public async Task AddIssuerKeyAsync_SendsAChallengeThenAnAddKeyRequest()
    {
        var keyId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""
            { "key_id": "{{keyId}}", "algorithm": "ed25519", "role": "operational", "purpose": "attestation",
              "valid_from": "2026-01-01T00:00:00Z", "valid_until": null, "revoked_at": null }
            """);
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var key = await session.AddIssuerKeyAsync("ed25519", Convert.ToBase64String(new byte[32]), "operational");

        Assert.Equal(keyId, key.KeyId);
        Assert.Contains("/integrations/dragons-inc/keys", handler.Requests[1].Url);
    }

    [Fact]
    public async Task ListIssuerKeysAsync_IsPublic_NoAuthorizationHeaderSent()
    {
        var handler = new StubHttpMessageHandler().Enqueue("[]");
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var keys = await session.ListIssuerKeysAsync("dragons-inc");

        Assert.Empty(keys);
        Assert.Equal(HttpMethod.Get, handler.Requests[0].Method);
    }
}
