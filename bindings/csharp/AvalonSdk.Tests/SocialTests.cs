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
            .Enqueue($@"[{{""identity_id"":""{other}"",""status"":""Online"",""playing"":null,""updated_at"":""2026-01-01T00:00:00Z""}}]");
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
}
