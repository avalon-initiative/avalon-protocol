using System;
using System.Net;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class AvalonClientTests
{
    [Fact]
    public void Construction_DoesNotThrow()
    {
        var config = new AvalonConfig("https://example.invalid", "test-key");
        var client = new AvalonClient(config);
        Assert.NotNull(client);
    }

    [Fact]
    public async Task AuthenticateAsync_PopulatesIdentityAndProfileFromMe()
    {
        var identityId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""
            {
                "identity_id": "{{identityId}}",
                "identity_created_at": "2026-01-01T00:00:00Z",
                "display_name": "dragon-friend",
                "avatar_url": "https://example.invalid/avatar.png",
                "bio": null,
                "favorite_genres": ["rpg", "strategy"],
                "pronouns": null,
                "banner_url": null,
                "status": null,
                "links": [],
                "timezone": null,
                "theme_color": null,
                "location": null,
                "main_guild": null
            }
            """)
            // GET /me/grants — no grants configured for this key id, treated as "no grants".
            .Enqueue(HttpStatusCode.NotFound, """{ "error": "not found", "code": "NOT_FOUND" }""");
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        var session = await client.AuthenticateAsync("a-token");

        Assert.Equal(identityId, session.Identity.Id);
        Assert.Equal(identityId, session.Profile.IdentityId);
        Assert.Equal("dragon-friend", session.Profile.DisplayName);
        Assert.Equal("https://example.invalid/avatar.png", session.Profile.AvatarUrl);
        Assert.Equal(new[] { Genre.Rpg, Genre.Strategy }, session.Profile.FavoriteGenres);
        Assert.Equal(identityId.ToString(), session.IdentityId);
    }

    [Fact]
    public async Task AuthenticateAsync_NonSuccessMeResponse_ThrowsAuthenticationFailed()
    {
        var handler = new StubHttpMessageHandler().Enqueue(HttpStatusCode.Unauthorized, """{ "error": "unauthorized" }""");
        var client = new AvalonClient(new AvalonConfig("https://example.invalid", "test-key"), handler.ToHttpClient());

        await Assert.ThrowsAsync<AuthenticationFailedException>(() => client.AuthenticateAsync("bad-token"));
    }
}
