// Live tests against a real, running avalon-server — mirrors
// crates/sdk/tests/social.rs, guilds.rs, conversations.rs, and
// achievements.rs, same scenarios translated to C#. Opt-in: every test here
// is a no-op unless AVALON_SERVER_URL is set (DATABASE_URL is also needed
// for the tests that seed identities/friendships directly via SQL — there
// is no SDK-level way to create one without a friend-request/accept flow or
// a full WebAuthn ceremony, same reason the Rust tests use sqlx/a virtual
// authenticator directly). Every seeded display name carries a fresh Guid
// suffix — this Postgres instance is shared and long-lived, so a fixed name
// collides with a previous run's row instead of the current one.
//
// DatabaseUrl must be an Npgsql-style keyword/value connection string
// (`Host=...;Username=...;Password=...;Database=...`), not the `postgres://`
// URI `.env`'s own DATABASE_URL uses for the Rust side — Npgsql doesn't
// parse the URI form.

using System;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Avalon.Sdk;
using Npgsql;
using Org.BouncyCastle.Crypto.Generators;
using Org.BouncyCastle.Crypto.Parameters;
using Org.BouncyCastle.Crypto.Signers;
using Org.BouncyCastle.Security;
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
        var (aliceId, aliceToken) = await SeedIdentitySessionAsync(conn, $"alice-social-{Guid.NewGuid():N}");
        var (bobId, _) = await SeedIdentitySessionAsync(conn, $"bob-social-{Guid.NewGuid():N}");
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
        var (_, token) = await SeedIdentitySessionAsync(conn, $"presence-self-{Guid.NewGuid():N}");

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
        var (_, token) = await SeedIdentitySessionAsync(conn, $"guild-owner-{Guid.NewGuid():N}");

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
        var (_, token) = await SeedIdentitySessionAsync(conn, $"guild-chatter-{Guid.NewGuid():N}");

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
        var (aliceId, aliceToken) = await SeedIdentitySessionAsync(conn, $"alice-dm-{Guid.NewGuid():N}");
        var (bobId, bobToken) = await SeedIdentitySessionAsync(conn, $"bob-dm-{Guid.NewGuid():N}");

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

    private sealed class RegisteredIntegrator
    {
        public byte[] SigningKeySeed { get; set; } = Array.Empty<byte>();
        public string Slug { get; set; } = "";
        public string KeyId { get; set; } = "";
    }

    /// <summary>POST /integrations — mirrors crates/sdk/tests/achievements.rs's
    /// register_integrator, using BouncyCastle's Ed25519 rather than ed25519-dalek.</summary>
    private static async Task<RegisteredIntegrator> RegisterIntegratorAsync(HttpClient http, string baseUrl)
    {
        var suffix = Guid.NewGuid().ToString("N");
        var random = new SecureRandom();
        var keyGen = new Ed25519KeyPairGenerator();
        keyGen.Init(new Ed25519KeyGenerationParameters(random));
        var keyPair = keyGen.GenerateKeyPair();
        var privateKey = (Ed25519PrivateKeyParameters)keyPair.Private;
        var publicKey = (Ed25519PublicKeyParameters)keyPair.Public;
        var slug = $"sdk-achv-{suffix.Substring(0, 10)}";

        var body = new
        {
            slug,
            name = $"SDK Achievements Test {suffix.Substring(0, 8)}",
            owner_name = "Test Studio",
            requested_capabilities = new[] { "achievements.issue", "achievements.read" },
            initial_key = new
            {
                algorithm = "ed25519",
                public_key = Convert.ToBase64String(publicKey.GetEncoded()),
            },
        };
        using var response = await http.PostAsync($"{baseUrl}/integrations",
            new StringContent(JsonSerializer.Serialize(body), Encoding.UTF8, "application/json"));
        response.EnsureSuccessStatusCode();
        using var doc = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
        return new RegisteredIntegrator
        {
            SigningKeySeed = privateKey.GetEncoded(),
            Slug = slug,
            KeyId = doc.RootElement.GetProperty("credential").GetProperty("key_id").GetString()!,
        };
    }

    /// <summary>The integrator challenge-response ceremony, shared by DefineAchievementAsync
    /// and by Session.IssueAchievementAsync itself (production code lives in Achievements.cs;
    /// this copy exists only to define the achievement ahead of issuing it, an integrator-owner
    /// action the SDK itself deliberately never exposes).</summary>
    private static async Task<(string ChallengeId, byte[] Signature)> ChallengeAsync(HttpClient http, string baseUrl, RegisteredIntegrator integrator)
    {
        using var response = await http.PostAsync($"{baseUrl}/integrations/{integrator.Slug}/challenge", null);
        response.EnsureSuccessStatusCode();
        using var doc = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
        var challengeId = doc.RootElement.GetProperty("challenge_id").GetString()!;
        var nonce = Convert.FromBase64String(doc.RootElement.GetProperty("nonce").GetString()!);

        var privateKey = new Ed25519PrivateKeyParameters(integrator.SigningKeySeed, 0);
        var signer = new Ed25519Signer();
        signer.Init(true, privateKey);
        signer.BlockUpdate(nonce, 0, nonce.Length);
        return (challengeId, signer.GenerateSignature());
    }

    private static async Task DefineAchievementAsync(HttpClient http, string baseUrl, RegisteredIntegrator integrator, string key)
    {
        var (challengeId, signature) = await ChallengeAsync(http, baseUrl, integrator);
        using var request = new HttpRequestMessage(HttpMethod.Post, $"{baseUrl}/integrations/{integrator.Slug}/achievements");
        request.Headers.Add("x-avalon-integrator-key-id", integrator.KeyId);
        request.Headers.Add("x-avalon-integrator-challenge-id", challengeId);
        request.Headers.Add("x-avalon-integrator-signature", Convert.ToBase64String(signature));
        request.Content = new StringContent(
            JsonSerializer.Serialize(new { key, name = "Dragon Slayer", description = "Slew the dragon" }),
            Encoding.UTF8, "application/json");
        using var response = await http.SendAsync(request);
        response.EnsureSuccessStatusCode();
    }

    private static async Task ConnectIntegratorAsync(HttpClient http, string baseUrl, string integratorSlug, string token, params string[] capabilities)
    {
        using var request = new HttpRequestMessage(HttpMethod.Post, $"{baseUrl}/integrations/{integratorSlug}/connect");
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
        request.Content = new StringContent(
            JsonSerializer.Serialize(new { capabilities }), Encoding.UTF8, "application/json");
        using var response = await http.SendAsync(request);
        response.EnsureSuccessStatusCode();
    }

    /// <summary>Mirrors crates/sdk/tests/achievements.rs's
    /// issue_achievement_then_read_it_back_via_the_sdk, end to end through AvalonClient/Session
    /// rather than raw HTTP for the issuance/read steps.</summary>
    [Fact]
    public async Task IssueAchievementThenReadItBack_RoundTripsThroughTheSdk()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (_, token) = await SeedIdentitySessionAsync(conn, $"sdk-achv-csharp-{Guid.NewGuid():N}");

        var http = new HttpClient();
        var integrator = await RegisterIntegratorAsync(http, ServerUrl!);
        await DefineAchievementAsync(http, ServerUrl!, integrator, "dragon_slayer");
        await ConnectIntegratorAsync(http, ServerUrl!, integrator.Slug, token, "achievements.issue", "achievements.read");

        var client = new AvalonClient(new AvalonConfig(
            ServerUrl!, integrator.KeyId, integrator.Slug, integrator.SigningKeySeed));
        var session = await client.AuthenticateAsync(token);

        var attestationId = await session.IssueAchievementAsync("dragon_slayer");
        var history = await session.GetAchievementsAsync();

        var attestation = Assert.Single(history);
        Assert.Equal(attestationId, attestation.Id);
        Assert.True(attestation.Authenticity.IsAuthentic);
        Assert.True(attestation.Validity.IsValid);
        Assert.Single(attestation.History);
        Assert.Equal("issued", attestation.History[0].Event);
    }

    /// <summary>Mirrors crates/sdk/tests/achievements.rs's
    /// issue_achievement_without_a_configured_signing_key_is_rejected — the SDK never even
    /// attempts an HTTP call without IntegratorSlug/SigningKey configured.</summary>
    [Fact]
    public async Task IssueAchievementAsync_WithoutConfiguredSigningKey_ThrowsWithoutCallingTheServer()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (_, token) = await SeedIdentitySessionAsync(conn, $"sdk-achv-nokey-csharp-{Guid.NewGuid():N}");

        var http = new HttpClient();
        var integrator = await RegisterIntegratorAsync(http, ServerUrl!);
        await DefineAchievementAsync(http, ServerUrl!, integrator, "dragon_slayer");
        await ConnectIntegratorAsync(http, ServerUrl!, integrator.Slug, token, "achievements.issue");

        // No IntegratorSlug/SigningKey configured.
        var client = new AvalonClient(new AvalonConfig(ServerUrl!, integrator.KeyId));
        var session = await client.AuthenticateAsync(token);

        await Assert.ThrowsAsync<MissingIssuerCredentialsException>(() => session.IssueAchievementAsync("dragon_slayer"));
    }

    /// <summary>Session.ForTesting is internal to keep the capability escape hatch out of the
    /// public SDK surface (same reasoning as the Rust SDK's grant_for_testing, gated behind
    /// its own test-util feature) — usable here because LiveTests.cs lives in this assembly's
    /// own InternalsVisibleTo test project, not because these tests exercise a real grant flow
    /// (issues #26-#28 aren't built yet, so every AuthenticateAsync today returns no grants).</summary>
    private static Session SessionForCapabilities(Session authenticated, params string[] capabilities) =>
        Session.ForTesting(capabilities, authenticated.Http, authenticated.ServerUrl, authenticated.Token, authenticated.IdentityGuid);
}
