using System;
using System.Linq;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class RegistryTests
{
    private static readonly byte[] TestSigningKey = Enumerable.Range(0, 32).Select(i => (byte)i).ToArray();

    [Fact]
    public async Task PublishRecognitionAsync_WithoutConfiguredSlug_ThrowsBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<MissingIssuerCredentialsException>(
            () => session.PublishRecognitionAsync("worldzero", new[] { "achievements" }));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task PublishRecognitionAsync_SendsAChallengeThenAPublishRequest()
    {
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue("""
            { "recognizer_slug": "dragons-inc", "recognized_slug": "worldzero", "scope": ["achievements"], "published_at": "2026-01-01T00:00:00Z" }
            """);
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var recognition = await session.PublishRecognitionAsync("worldzero", new[] { "achievements" });

        Assert.Equal("worldzero", recognition.RecognizedSlug);
        Assert.Contains("/integrations/dragons-inc/recognitions", handler.Requests[1].Url);
    }

    [Fact]
    public async Task RevokeRecognitionAsync_ReturnsWhetherAnythingWasRevoked()
    {
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue("""{ "revoked": true }""");
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var revoked = await session.RevokeRecognitionAsync("worldzero");

        Assert.True(revoked);
        Assert.Contains("/integrations/dragons-inc/recognitions/revoke", handler.Requests[1].Url);
    }

    [Fact]
    public async Task ListRecognitionsAsync_IsPublic_NoAuthorizationHeaderRequired()
    {
        var handler = new StubHttpMessageHandler().Enqueue("[]");
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var recognitions = await session.ListRecognitionsAsync("dragons-inc");

        Assert.Empty(recognitions);
        Assert.Equal(HttpMethod.Get, handler.Requests[0].Method);
    }

    [Fact]
    public async Task GetIntegratorRegistryAsync_ParsesEachMetricsExactFlag()
    {
        var handler = new StubHttpMessageHandler().Enqueue("""
        {
            "players": { "value": 42, "definition": "distinct players", "class": "engagement", "exact": true },
            "total_players_ever": { "value": 5, "definition": "floor", "class": "engagement", "exact": false },
            "achievements_issued": { "value": 100, "definition": "issued", "class": "activity", "exact": true },
            "achievements_revoked": { "value": 1, "definition": "revoked", "class": "activity", "exact": true },
            "unique_achievement_holders": { "value": 40, "definition": "holders", "class": "engagement", "exact": true }
        }
        """);
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var registry = await session.GetIntegratorRegistryAsync("dragons-inc");

        Assert.Equal(42, registry.Players.Value);
        Assert.True(registry.Players.Exact);
        Assert.False(registry.TotalPlayersEver.Exact);
    }
}
