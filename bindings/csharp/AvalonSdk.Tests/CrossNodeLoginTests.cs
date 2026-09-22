using System;
using System.Net;
using System.Net.Http;
using System.Threading.Tasks;
using Avalon.Sdk;
using Org.BouncyCastle.Crypto.Generators;
using Org.BouncyCastle.Crypto.Parameters;
using Org.BouncyCastle.Security;
using Xunit;

namespace Avalon.Sdk.Tests;

/// <summary>
/// Stubbed-HTTP unit tests for CrossNodeLogin/AvalonClient's cross-node-login methods
/// (epic #623, issue #638) — mirrors crates/sdk/tests/cross_node_login.rs's orchestration
/// assertions (what gets called, in what order, with what request/response shape), without a
/// real server. Live, end-to-end signature-verification coverage lives in LiveTests.cs.
/// </summary>
public class CrossNodeLoginTests
{
    private const string MeResponseBody = """
        {
            "identity_id": "11111111-1111-1111-1111-111111111111",
            "identity_created_at": "2026-01-01T00:00:00Z",
            "display_name": "dragon-friend",
            "avatar_url": null,
            "bio": null,
            "favorite_genres": [],
            "pronouns": null,
            "banner_url": null,
            "status": null,
            "links": [],
            "timezone": null,
            "theme_color": null,
            "location": null,
            "main_guild": null
        }
        """;

    private const string NoGrantsBody = """{ "error": "not found", "code": "NOT_FOUND" }""";

    [Fact]
    public async Task CrossNodeLoginAsync_PopulatesFieldsFromStartResponse()
    {
        var handler = new StubHttpMessageHandler().Enqueue("""
            {
                "request_code": "opaque-request-code",
                "user_code": "ABCD1234",
                "requesting_context": "https://node-b.example",
                "expires_in": 600,
                "poll_interval": 5
            }
            """);
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var pending = await client.CrossNodeLoginAsync();

        Assert.Equal("ABCD1234", pending.UserCode);
        Assert.Equal("https://node-b.example", pending.RequestingContext);
        Assert.Equal(600, pending.ExpiresIn);
        Assert.Equal(HttpMethod.Post, handler.Requests[0].Method);
        Assert.Equal("https://example.invalid/auth/cross-node/start", handler.Requests[0].Url);
    }

    [Fact]
    public async Task WaitAsync_ResolvesToASessionOnApproved()
    {
        var handler = new StubHttpMessageHandler()
            .Enqueue("""
                {
                    "request_code": "opaque-request-code",
                    "user_code": "ABCD1234",
                    "requesting_context": "https://example.invalid",
                    "expires_in": 600,
                    "poll_interval": 1
                }
                """)
            .Enqueue("""{ "status": "approved", "token": "a-real-session-token" }""")
            .Enqueue(MeResponseBody)
            .Enqueue(HttpStatusCode.NotFound, NoGrantsBody);
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var pending = await client.CrossNodeLoginAsync();
        var session = await pending.WaitAsync();

        Assert.Equal("dragon-friend", session.Profile.DisplayName);
        // The poll call authenticates with the opaque request_code, not a real session token.
        Assert.Equal("opaque-request-code", handler.Requests[1].AuthorizationToken);
        Assert.Equal("https://example.invalid/auth/cross-node/poll", handler.Requests[1].Url);
    }

    [Fact]
    public async Task WaitAsync_ThrowsDeniedOnDenied()
    {
        var handler = new StubHttpMessageHandler()
            .Enqueue("""
                {
                    "request_code": "opaque-request-code",
                    "user_code": "ABCD1234",
                    "requesting_context": "https://example.invalid",
                    "expires_in": 600,
                    "poll_interval": 1
                }
                """)
            .Enqueue("""{ "status": "denied", "token": null }""");
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var pending = await client.CrossNodeLoginAsync();
        await Assert.ThrowsAsync<CrossNodeLoginDeniedException>(() => pending.WaitAsync());
    }

    [Fact]
    public async Task WaitAsync_ThrowsExpiredOnExpired()
    {
        var handler = new StubHttpMessageHandler()
            .Enqueue("""
                {
                    "request_code": "opaque-request-code",
                    "user_code": "ABCD1234",
                    "requesting_context": "https://example.invalid",
                    "expires_in": 600,
                    "poll_interval": 1
                }
                """)
            .Enqueue("""{ "status": "expired", "token": null }""");
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var pending = await client.CrossNodeLoginAsync();
        await Assert.ThrowsAsync<CrossNodeLoginExpiredException>(() => pending.WaitAsync());
    }

    [Fact]
    public async Task WaitAsync_RetriesOnPendingAndSlowDownBeforeApproving()
    {
        var handler = new StubHttpMessageHandler()
            .Enqueue("""
                {
                    "request_code": "opaque-request-code",
                    "user_code": "ABCD1234",
                    "requesting_context": "https://example.invalid",
                    "expires_in": 600,
                    "poll_interval": 1
                }
                """)
            .Enqueue("""{ "status": "pending", "token": null }""")
            .Enqueue("""{ "status": "slow_down", "token": null }""")
            .Enqueue("""{ "status": "approved", "token": "a-real-session-token" }""")
            .Enqueue(MeResponseBody)
            .Enqueue(HttpStatusCode.NotFound, NoGrantsBody);
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var pending = await client.CrossNodeLoginAsync();
        var session = await pending.WaitAsync();

        Assert.Equal("dragon-friend", session.Profile.DisplayName);
        // start + 3 polls (pending, slow_down, approved) + /me + /me/grants.
        Assert.Equal(6, handler.Requests.Count);
    }

    [Fact]
    public async Task SubmitCrossNodeLoginGrantAsync_PostsASignedGrantAndResolvesToASession()
    {
        var handler = new StubHttpMessageHandler()
            .Enqueue("""{ "token": "a-real-session-token" }""")
            .Enqueue(MeResponseBody)
            .Enqueue(HttpStatusCode.NotFound, NoGrantsBody);
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var generator = new Ed25519KeyPairGenerator();
        generator.Init(new Ed25519KeyGenerationParameters(new SecureRandom()));
        var keyPair = generator.GenerateKeyPair();
        var privateKey = (Ed25519PrivateKeyParameters)keyPair.Private;

        var session = await client.SubmitCrossNodeLoginGrantAsync(
            Guid.NewGuid(), Guid.NewGuid(), privateKey.GetEncoded());

        Assert.Equal("dragon-friend", session.Profile.DisplayName);
        Assert.Equal(HttpMethod.Post, handler.Requests[0].Method);
        Assert.Equal("https://example.invalid/auth/cross-node/submit", handler.Requests[0].Url);
    }

    [Fact]
    public async Task LookupCrossNodeLoginAsync_IsPublic_NoAuthorizationHeaderSent()
    {
        var handler = new StubHttpMessageHandler().Enqueue("""
        { "status": "pending", "requesting_context": "https://requester.example", "expires_in": 300,
          "integrator_verified": false, "display_name": null }
        """);
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var lookup = await client.LookupCrossNodeLoginAsync("ABCD-1234");

        Assert.Equal("pending", lookup.Status);
        Assert.Null(handler.Requests[0].AuthorizationToken);
        Assert.Contains("/auth/cross-node/lookup", handler.Requests[0].Url);
        Assert.Contains("user_code=ABCD-1234", handler.Requests[0].Url);
    }

    [Fact]
    public async Task DenyCrossNodeLoginAsync_PostsTheUserCode()
    {
        var handler = new StubHttpMessageHandler().Enqueue("""{ "status": "denied" }""");
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        await client.DenyCrossNodeLoginAsync("ABCD-1234");

        Assert.Equal(HttpMethod.Post, handler.Requests[0].Method);
        Assert.Equal("https://example.invalid/auth/cross-node/deny", handler.Requests[0].Url);
        Assert.Contains("ABCD-1234", handler.Requests[0].Body);
    }
}
