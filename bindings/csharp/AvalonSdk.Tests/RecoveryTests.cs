using System;
using System.Text.Json;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class RecoveryTests
{
    [Fact]
    public async Task StartRecoveryAsync_NoBearerToken_ReturnsTicketAndChallenge()
    {
        var ticketId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        { "ticket_id": "{{ticketId}}", "challenge": { "publicKey": { "challenge": "abc" } } }
        """);
        var client = new AvalonClient(new AvalonConfig("http://test", "unused"), handler.ToHttpClient());

        var (returnedTicketId, challenge) = await client.StartRecoveryAsync(Guid.NewGuid());

        Assert.Equal(ticketId, returnedTicketId);
        Assert.Equal(JsonValueKind.Object, challenge.ValueKind);
        Assert.Null(handler.Requests[0].AuthorizationToken);
        Assert.Contains("/recovery/requests/start", handler.Requests[0].Url);
    }

    [Fact]
    public async Task FinishRecoveryAsync_PostsTicketAndCredential()
    {
        var requestId = Guid.NewGuid();
        var identityId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        { "id": "{{requestId}}", "identity_id": "{{identityId}}", "status": "pending_approvals",
          "threshold": 2, "approvals_count": 0, "requested_at": "2026-01-01T00:00:00Z", "delay_ends_at": null }
        """);
        var client = new AvalonClient(new AvalonConfig("http://test", "unused"), handler.ToHttpClient());
        var credential = JsonDocument.Parse("""{ "id": "cred" }""").RootElement;

        var request = await client.FinishRecoveryAsync(Guid.NewGuid(), credential);

        Assert.Equal(requestId, request.Id);
        Assert.Equal("pending_approvals", request.Status);
        Assert.Contains("/recovery/requests/finish", handler.Requests[0].Url);
    }

    [Fact]
    public async Task GetRecoveryRequestAsync_IsPublic_NoAuthorizationHeaderSent()
    {
        var requestId = Guid.NewGuid();
        var identityId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        { "id": "{{requestId}}", "identity_id": "{{identityId}}", "status": "delay",
          "threshold": 2, "approvals_count": 2, "requested_at": "2026-01-01T00:00:00Z", "delay_ends_at": "2026-01-03T00:00:00Z" }
        """);
        var client = new AvalonClient(new AvalonConfig("http://test", "unused"), handler.ToHttpClient());

        var request = await client.GetRecoveryRequestAsync(requestId);

        Assert.Equal("delay", request.Status);
        Assert.Null(handler.Requests[0].AuthorizationToken);
    }

    [Fact]
    public async Task GetIdentityRecoveryStatusAsync_NoActiveRecovery_ReturnsNull()
    {
        var handler = new StubHttpMessageHandler().Enqueue("null");
        var client = new AvalonClient(new AvalonConfig("http://test", "unused"), handler.ToHttpClient());

        var status = await client.GetIdentityRecoveryStatusAsync(Guid.NewGuid());

        Assert.Null(status);
    }

    [Fact]
    public async Task FinalizeRecoveryRequestAsync_PostsToFinalize()
    {
        var requestId = Guid.NewGuid();
        var identityId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        { "id": "{{requestId}}", "identity_id": "{{identityId}}", "status": "completed",
          "threshold": 2, "approvals_count": 2, "requested_at": "2026-01-01T00:00:00Z", "delay_ends_at": "2026-01-03T00:00:00Z" }
        """);
        var client = new AvalonClient(new AvalonConfig("http://test", "unused"), handler.ToHttpClient());

        var request = await client.FinalizeRecoveryRequestAsync(requestId);

        Assert.Equal("completed", request.Status);
        Assert.Equal(HttpMethod.Post, handler.Requests[0].Method);
        Assert.Contains($"/recovery/requests/{requestId}/finalize", handler.Requests[0].Url);
    }
}
