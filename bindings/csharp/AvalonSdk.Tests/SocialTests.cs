using System;
using System.Linq;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class SocialTests
{
    [Fact]
    public async Task FriendsAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.FriendsAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task PresenceAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.PresenceAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task PresenceOfAsync_WithNoIds_ShortCircuitsBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(new[] { "presence.read" }, handler.ToHttpClient());

        var result = await session.PresenceOfAsync(Array.Empty<Guid>());

        Assert.Empty(result);
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task FriendsAsync_ReturnsFriendsWithoutPresence_WhenPresenceReadNotGranted()
    {
        var self = Guid.NewGuid();
        var other = Guid.NewGuid();
        var handler = new StubHttpMessageHandler()
            .Enqueue($@"[{{""a"":""{self}"",""b"":""{other}"",""since"":""2026-01-01T00:00:00Z""}}]");
        var session = Session.ForTesting(new[] { "friends.read" }, handler.ToHttpClient(), identityId: self);

        var friends = await session.FriendsAsync();

        var friend = Assert.Single(friends);
        Assert.Equal(other, friend.IdentityId);
        Assert.Null(friend.Presence);
        // friends.read alone must not also fetch presence.
        Assert.Single(handler.Requests);
    }

    [Fact]
    public async Task FriendsAsync_EmbedsPresence_WhenPresenceReadAlsoGranted()
    {
        var self = Guid.NewGuid();
        var other = Guid.NewGuid();
        var handler = new StubHttpMessageHandler()
            .Enqueue($@"[{{""a"":""{self}"",""b"":""{other}"",""since"":""2026-01-01T00:00:00Z""}}]")
            .Enqueue($@"[{{""identity_id"":""{other}"",""status"":""Online"",""active_in"":null,""updated_at"":""2026-01-01T00:00:00Z""}}]");
        var session = Session.ForTesting(new[] { "friends.read", "presence.read" }, handler.ToHttpClient(), identityId: self);

        var friends = await session.FriendsAsync();

        var friend = Assert.Single(friends);
        Assert.NotNull(friend.Presence);
        Assert.Equal(PresenceStatus.Online, friend.Presence!.Status);
        Assert.Equal(2, handler.Requests.Count);
    }

    [Fact]
    public async Task UpdatePresenceAsync_SendsPutWithBearerToken()
    {
        var handler = new StubHttpMessageHandler().Enqueue("{}");
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient(), token: "the-token");

        await session.UpdatePresenceAsync(PresenceStatus.Away);

        var request = Assert.Single(handler.Requests);
        Assert.Equal(System.Net.Http.HttpMethod.Put, request.Method);
        Assert.EndsWith("/me/presence", request.Url);
        Assert.Equal("the-token", request.AuthorizationToken);
    }

    [Fact]
    public async Task UpdateIntegratorPresenceAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(
            () => session.UpdateIntegratorPresenceAsync(Guid.NewGuid(), PresenceStatus.Online));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task UpdateIntegratorPresenceAsync_SendsAChallengeThenAPutRequest()
    {
        var identityId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""{ "identity_id": "{{identityId}}", "status": "Online", "active_in": null, "updated_at": "2026-01-01T00:00:00Z" }""");
        var session = Session.ForTesting(
            new[] { "presence.publish" },
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: Enumerable.Range(0, 32).Select(i => (byte)i).ToArray());

        var presence = await session.UpdateIntegratorPresenceAsync(identityId, PresenceStatus.Online);

        Assert.Equal(identityId, presence.IdentityId);
        Assert.Equal(HttpMethod.Put, handler.Requests[1].Method);
        Assert.Contains($"/presence/{identityId}", handler.Requests[1].Url);
    }

    [Fact]
    public async Task GetLocationsAsync_IsPublic_ReturnsShardBaseUrls()
    {
        var handler = new StubHttpMessageHandler().Enqueue("""{ "locations": ["https://node-a.example", "https://node-b.example"] }""");
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var locations = await session.GetLocationsAsync(Guid.NewGuid());

        Assert.Equal(2, locations.Count);
        Assert.Contains("https://node-a.example", locations);
    }
}
