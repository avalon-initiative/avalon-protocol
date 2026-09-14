// Live tests against a real, running avalon-server — mirrors
// crates/sdk/tests/social.rs, guilds.rs, and conversations.rs, same
// scenarios translated to C#. Opt-in: every test here is a no-op unless
// AVALON_SERVER_URL is set (DATABASE_URL is also needed for the social
// tests, which seed a friendship directly via SQL — there is no
// SDK-level way to create one without a friend-request/accept flow, same
// reason the Rust tests use sqlx directly). Not run in this environment;
// see the PR description.

using System;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Avalon.Sdk;
using Npgsql;
using Xunit;

namespace Avalon.Sdk.Tests;

public class LiveTests
{
    private static string? ServerUrl => Environment.GetEnvironmentVariable("AVALON_SERVER_URL");
    private static string? DatabaseUrl => Environment.GetEnvironmentVariable("DATABASE_URL");

    private static AvalonClient Client() => new(new AvalonConfig(ServerUrl!, "sdk-test"));

    private static async Task<(Guid IdentityId, string Token)> SeedIdentitySessionAsync(NpgsqlConnection conn, string displayName)
    {
        var identityId = Guid.NewGuid();
        await using (var cmd = new NpgsqlCommand("INSERT INTO identities (id) VALUES ($1)", conn))
        {
            cmd.Parameters.AddWithValue(identityId);
            await cmd.ExecuteNonQueryAsync();
        }
        await using (var cmd = new NpgsqlCommand("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)", conn))
        {
            cmd.Parameters.AddWithValue(identityId);
            cmd.Parameters.AddWithValue(displayName);
            await cmd.ExecuteNonQueryAsync();
        }
        var token = $"test-token-{Guid.NewGuid()}";
        await using (var cmd = new NpgsqlCommand("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)", conn))
        {
            cmd.Parameters.AddWithValue(token);
            cmd.Parameters.AddWithValue(identityId);
            cmd.Parameters.AddWithValue(DateTimeOffset.UtcNow.AddHours(1));
            await cmd.ExecuteNonQueryAsync();
        }
        return (identityId, token);
    }

    /// <summary>Presence reads default to friends-only visibility, so any test checking one
    /// identity's view of another's real presence needs this first.</summary>
    private static async Task SeedFriendshipAsync(NpgsqlConnection conn, Guid x, Guid y)
    {
        var (a, b) = x.CompareTo(y) < 0 ? (x, y) : (y, x);
        await using var cmd = new NpgsqlCommand("INSERT INTO friendships (a, b) VALUES ($1, $2)", conn);
        cmd.Parameters.AddWithValue(a);
        cmd.Parameters.AddWithValue(b);
        await cmd.ExecuteNonQueryAsync();
    }

    private static async Task<Guid> CreateGuildAsync(HttpClient http, string baseUrl, string token, string tag)
    {
        using var request = new HttpRequestMessage(HttpMethod.Post, $"{baseUrl}/guilds");
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
        request.Content = new StringContent(
            System.Text.Json.JsonSerializer.Serialize(new { name = $"Guild {tag}", tag, description = "" }),
            Encoding.UTF8, "application/json");
        using var response = await http.SendAsync(request);
        response.EnsureSuccessStatusCode();
        using var doc = System.Text.Json.JsonDocument.Parse(await response.Content.ReadAsStringAsync());
        return doc.RootElement.GetProperty("id").GetGuid();
    }

    [Fact]
    public async Task FriendsAsync_ReturnsAFriendshipSeededDirectly()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (aliceId, aliceToken) = await SeedIdentitySessionAsync(conn, "alice-social");
        var (bobId, _) = await SeedIdentitySessionAsync(conn, "bob-social");
        await SeedFriendshipAsync(conn, aliceId, bobId);

        var session = await Client().AuthenticateAsync(aliceToken);
        var granted = SessionForCapabilities(session, "friends.read");
        var friends = await granted.FriendsAsync();

        Assert.Contains(friends, f => f.IdentityId == bobId);
    }

    [Fact]
    public async Task UpdatePresenceThenPresence_RoundTrips()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (_, token) = await SeedIdentitySessionAsync(conn, "presence-self");

        var session = await Client().AuthenticateAsync(token);
        var granted = SessionForCapabilities(session, "presence.read");

        await granted.UpdatePresenceAsync(PresenceStatus.Away);
        var mine = await granted.PresenceAsync();

        Assert.Equal(PresenceStatus.Away, mine.Status);
    }

    [Fact]
    public async Task GuildsAsync_ListsAMembershipCreatedViaTheHttpApi()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (_, token) = await SeedIdentitySessionAsync(conn, "guild-owner");

        var session = await Client().AuthenticateAsync(token);
        var granted = SessionForCapabilities(session, "guilds.read");
        var guildId = await CreateGuildAsync(new HttpClient(), ServerUrl!, token, $"tag{Guid.NewGuid():N}".Substring(0, 8));

        var memberships = await granted.GuildsAsync();

        Assert.Contains(memberships, m => m.Guild.Id == guildId);
    }

    [Fact]
    public async Task SendThenMessages_RoundTripsThroughTheDefaultGeneralChannel()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (_, token) = await SeedIdentitySessionAsync(conn, "guild-chatter");

        var http = new HttpClient();
        var guildId = await CreateGuildAsync(http, ServerUrl!, token, $"tag{Guid.NewGuid():N}".Substring(0, 8));

        var session = await Client().AuthenticateAsync(token);
        var granted = SessionForCapabilities(session, "guilds.read", "guilds.chat");
        var channels = await granted.Guild(guildId).ChannelsAsync();
        var general = Assert.Single(channels);

        var channel = granted.Guild(guildId).Channel(general.Id);
        var sent = await channel.SendAsync("hello guild");
        var messages = await channel.MessagesAsync();

        Assert.Contains(messages, m => m.Id == sent.Id);
    }

    [Fact]
    public async Task DmThenSendThenMessages_RoundTripsAcrossTwoSessions()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (aliceId, aliceToken) = await SeedIdentitySessionAsync(conn, "alice-dm");
        var (bobId, bobToken) = await SeedIdentitySessionAsync(conn, "bob-dm");

        var aliceSession = SessionForCapabilities(await Client().AuthenticateAsync(aliceToken), "messages.read", "messages.send");
        var bobSession = SessionForCapabilities(await Client().AuthenticateAsync(bobToken), "messages.read", "messages.send");

        var aliceHandle = await aliceSession.DmAsync(bobId);
        await aliceHandle.SendAsync("hi bob");

        var bobConversations = await bobSession.ConversationsAsync();
        var bobConversation = Assert.Single(bobConversations);
        var bobHandle = bobSession.Conversation(bobConversation.Id);
        var reply = await bobHandle.SendAsync("hi alice");

        var aliceMessages = await aliceHandle.MessagesAsync();
        Assert.Contains(aliceMessages, m => m.Id == reply.Id);
    }

    /// <summary>Session.ForTesting is internal to keep the capability escape hatch out of the
    /// public SDK surface (same reasoning as the Rust SDK's grant_for_testing, gated behind
    /// its own test-util feature) — usable here because LiveTests.cs lives in this assembly's
    /// own InternalsVisibleTo test project, not because these tests exercise a real grant flow
    /// (issues #26-#28 aren't built yet, so every AuthenticateAsync today returns no grants).</summary>
    private static Session SessionForCapabilities(Session authenticated, params string[] capabilities) =>
        Session.ForTesting(capabilities, authenticated.Http, authenticated.ServerUrl, authenticated.Token, authenticated.IdentityGuid);
}
