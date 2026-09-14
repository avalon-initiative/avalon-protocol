using System;
using System.Net;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class ConversationsTests
{
    [Fact]
    public async Task ConversationsAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.ConversationsAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task DmAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.DmAsync(Guid.NewGuid()));
        Assert.Empty(handler.Requests);
    }

    /// <summary>messages.read alone must not satisfy DmAsync/SendAsync, and messages.send
    /// alone must not satisfy ConversationsAsync/MessagesAsync — no messages.* blanket check,
    /// mirroring guilds.read not satisfying guilds.chat.</summary>
    [Fact]
    public async Task MessagesReadAndMessagesSend_DoNotSatisfyEachOther()
    {
        var handler = new StubHttpMessageHandler();
        var readOnly = Session.ForTesting(new[] { "messages.read" }, handler.ToHttpClient());
        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => readOnly.DmAsync(Guid.NewGuid()));

        var sendOnly = Session.ForTesting(new[] { "messages.send" }, handler.ToHttpClient());
        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => sendOnly.ConversationsAsync());

        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task DmAsync_NotForbidden_ReturnsAHandleThatCanSend()
    {
        var conversationId = Guid.NewGuid();
        var messageId = Guid.NewGuid();
        var authorId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler()
            .Enqueue($@"{{""id"":""{conversationId}"",""participants"":[""{authorId}"",""{Guid.NewGuid()}""]}}")
            .Enqueue($@"{{""id"":""{messageId}"",""conversation_id"":""{conversationId}"",""author"":""{authorId}"",""body"":""hi"",""sent_at"":""2026-01-01T00:00:00Z""}}");
        var session = Session.ForTesting(new[] { "messages.send" }, handler.ToHttpClient());

        var handle = await session.DmAsync(Guid.NewGuid());
        var sent = await handle.SendAsync("hi");

        Assert.Equal(conversationId, handle.ConversationId);
        Assert.Equal("hi", sent.Body);
    }

    [Fact]
    public async Task ForbiddenResponse_MapsToNotConversationParticipant()
    {
        var handler = new StubHttpMessageHandler().Enqueue(HttpStatusCode.Forbidden, "{}");
        var session = Session.ForTesting(new[] { "messages.read" }, handler.ToHttpClient());

        await Assert.ThrowsAsync<NotConversationParticipantException>(
            () => session.Conversation(Guid.NewGuid()).MessagesAsync());
    }

    [Fact]
    public async Task OtherErrorStatus_StaysGeneric()
    {
        var handler = new StubHttpMessageHandler().Enqueue(HttpStatusCode.NotFound, "{}");
        var session = Session.ForTesting(new[] { "messages.read" }, handler.ToHttpClient());

        var ex = await Assert.ThrowsAsync<AvalonServerException>(
            () => session.Conversation(Guid.NewGuid()).MessagesAsync());
        Assert.Equal(HttpStatusCode.NotFound, ex.StatusCode);
    }
}
