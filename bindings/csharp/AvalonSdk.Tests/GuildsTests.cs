using System;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class GuildsTests
{
    [Fact]
    public async Task GuildsAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.GuildsAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task RosterAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.Guild(Guid.NewGuid()).RosterAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task ChannelsAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.Guild(Guid.NewGuid()).ChannelsAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task SendAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(
            () => session.Guild(Guid.NewGuid()).Channel(Guid.NewGuid()).SendAsync("hello"));
        Assert.Empty(handler.Requests);
    }

    /// <summary>guilds.read alone (no guilds.chat) must not satisfy ChannelsAsync/SendAsync —
    /// there is no guilds.* blanket check, mirroring the Rust SDK's own test.</summary>
    [Fact]
    public async Task GuildsRead_DoesNotSatisfyGuildsChatMethods()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(new[] { "guilds.read" }, handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.Guild(Guid.NewGuid()).ChannelsAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task RosterAsync_ReturnsMembersWithoutPresence_WhenPresenceReadNotGranted()
    {
        var guildId = Guid.NewGuid();
        var memberId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler()
            .Enqueue($@"[{{""guild_id"":""{guildId}"",""identity_id"":""{memberId}"",""role_index"":0,""joined_at"":""2026-01-01T00:00:00Z""}}]");
        var session = Session.ForTesting(new[] { "guilds.read" }, handler.ToHttpClient());

        var roster = await session.Guild(guildId).RosterAsync();

        var entry = Assert.Single(roster);
        Assert.Equal(memberId, entry.Member.IdentityId);
        Assert.Null(entry.Presence);
        Assert.Single(handler.Requests);
    }

    [Fact]
    public async Task SendThenMessages_RoundTripsAGuildMessage()
    {
        var guildId = Guid.NewGuid();
        var channelId = Guid.NewGuid();
        var messageId = Guid.NewGuid();
        var authorId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler()
            .Enqueue($@"{{""id"":""{messageId}"",""channel_id"":""{channelId}"",""author"":""{authorId}"",""body"":""hi guild"",""sent_at"":""2026-01-01T00:00:00Z""}}")
            .Enqueue($@"[{{""id"":""{messageId}"",""channel_id"":""{channelId}"",""author"":""{authorId}"",""body"":""hi guild"",""sent_at"":""2026-01-01T00:00:00Z""}}]");
        var session = Session.ForTesting(new[] { "guilds.chat" }, handler.ToHttpClient());
        var channel = session.Guild(guildId).Channel(channelId);

        var sent = await channel.SendAsync("hi guild");
        var messages = await channel.MessagesAsync();

        Assert.Equal("hi guild", sent.Body);
        var received = Assert.Single(messages);
        Assert.Equal(sent.Id, received.Id);
    }
}
