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
using System.Collections.Generic;
using System.Linq;
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

    /// <summary>#697/#698: POST /integrations/{slug}/connect is signature-required — seeds a
    /// real identity_signing_keys row so ConnectIntegratorAsync below can produce a genuine
    /// fresh signature, same pattern crates/cli/tests/issue_achievement.rs's Rust
    /// equivalent uses.</summary>
    private static async Task<(Guid KeyId, Ed25519PrivateKeyParameters PrivateKey)> SeedSigningKeyAsync(NpgsqlConnection conn, Guid identityId)
    {
        var privateKey = new Ed25519PrivateKeyParameters(new SecureRandom());
        var publicKey = privateKey.GeneratePublicKey().GetEncoded();
        await using var cmd = new NpgsqlCommand(
            "INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id", conn);
        cmd.Parameters.AddWithValue(identityId);
        cmd.Parameters.AddWithValue(publicKey);
        var keyId = (Guid)(await cmd.ExecuteScalarAsync())!;
        return (keyId, privateKey);
    }

    /// <summary>Presence reads default to friends-only visibility, so any test checking one
    /// identity's view of another's real presence needs this first. Writes
    /// `indexer_friendships` directly, not the old `friendships` table — that table has been
    /// dead since issue #506 retargeted `crates/server/src/friends.rs` to read the indexer
    /// projection instead, and a row seeded there is invisible to every read path now.</summary>
    private static async Task SeedFriendshipAsync(NpgsqlConnection conn, Guid x, Guid y)
    {
        var (a, b) = x.CompareTo(y) < 0 ? (x, y) : (y, x);
        await using var cmd = new NpgsqlCommand(
            "INSERT INTO indexer_friendships (a, b, since) VALUES ($1, $2, now())", conn);
        cmd.Parameters.AddWithValue(a);
        cmd.Parameters.AddWithValue(b);
        await cmd.ExecuteNonQueryAsync();
    }

    /// <summary>A fresh, unique guild tag respecting the server's 2-5 character bound
    /// (`crates/server/src/guilds.rs::validate_tag`) — pure hex, no prefix, so all 5
    /// characters carry real entropy against this shared, long-lived Postgres instance.</summary>
    private static string FreshGuildTag() => Guid.NewGuid().ToString("N").Substring(0, 5);

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
        var guildId = await CreateGuildAsync(new HttpClient(), ServerUrl!, token, FreshGuildTag());

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
        var guildId = await CreateGuildAsync(http, ServerUrl!, token, FreshGuildTag());

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
        // Issue #269: conversation creation requires an existing relationship
        // between every participant.
        await SeedFriendshipAsync(conn, aliceId, bobId);

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

    // Issue #742: conversations::SendMessageRequest and
    // guild_messages::SendMessageRequest used to collide under the same bare
    // schema name, silently dropping client_entry_id from the published
    // schema for POST /conversations/{id}/messages. This proves the fix
    // actually round-trips: a retried send with the same client_entry_id
    // dedupes server-side instead of posting a duplicate message.
    [Fact]
    public async Task SendWithClientEntryId_RetryDedupesInsteadOfDuplicating()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (aliceId, aliceToken) = await SeedIdentitySessionAsync(conn, $"alice-dedupe-{Guid.NewGuid():N}");
        var (bobId, _) = await SeedIdentitySessionAsync(conn, $"bob-dedupe-{Guid.NewGuid():N}");
        await SeedFriendshipAsync(conn, aliceId, bobId);

        var aliceSession = SessionForCapabilities(await Client().AuthenticateAsync(aliceToken), "messages.read", "messages.send");
        var aliceHandle = await aliceSession.DmAsync(bobId);

        var clientEntryId = Guid.NewGuid();
        var first = await aliceHandle.SendWithClientEntryIdAsync("retry me", clientEntryId);
        var retried = await aliceHandle.SendWithClientEntryIdAsync("retry me", clientEntryId);
        Assert.Equal(first.Id, retried.Id);

        var messages = await aliceHandle.MessagesAsync();
        Assert.Single(messages, m => m.Body == "retry me");
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

    private static async Task ConnectIntegratorAsync(HttpClient http, string baseUrl, string integratorSlug, string token, Guid identitySigningKeyId, Ed25519PrivateKeyParameters identitySigningKey, params string[] capabilities)
    {
        // Must match crates/server/src/signature_gate.rs::canonical_message
        // byte-for-byte: avalon:integration.connect:v1:<slug>:<capabilities joined by ",">.
        var message = Encoding.UTF8.GetBytes($"avalon:integration.connect:v1:{integratorSlug}:{string.Join(",", capabilities)}");
        var signer = new Ed25519Signer();
        signer.Init(true, identitySigningKey);
        signer.BlockUpdate(message, 0, message.Length);
        var signature = Convert.ToBase64String(signer.GenerateSignature());

        using var request = new HttpRequestMessage(HttpMethod.Post, $"{baseUrl}/integrations/{integratorSlug}/connect");
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
        request.Content = new StringContent(
            JsonSerializer.Serialize(new { capabilities, signing_key_id = identitySigningKeyId, signature }),
            Encoding.UTF8, "application/json");
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
        var (identityId, token) = await SeedIdentitySessionAsync(conn, $"sdk-achv-csharp-{Guid.NewGuid():N}");
        var (signingKeyId, signingKey) = await SeedSigningKeyAsync(conn, identityId);

        var http = new HttpClient();
        var integrator = await RegisterIntegratorAsync(http, ServerUrl!);
        await DefineAchievementAsync(http, ServerUrl!, integrator, "dragon_slayer");
        await ConnectIntegratorAsync(http, ServerUrl!, integrator.Slug, token, signingKeyId, signingKey, "achievements.issue", "achievements.read");

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
        var (identityId, token) = await SeedIdentitySessionAsync(conn, $"sdk-achv-nokey-csharp-{Guid.NewGuid():N}");
        var (signingKeyId, signingKey) = await SeedSigningKeyAsync(conn, identityId);

        var http = new HttpClient();
        var integrator = await RegisterIntegratorAsync(http, ServerUrl!);
        await DefineAchievementAsync(http, ServerUrl!, integrator, "dragon_slayer");
        await ConnectIntegratorAsync(http, ServerUrl!, integrator.Slug, token, signingKeyId, signingKey, "achievements.issue");

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

    /// <summary>Seeds an identity plus a real Ed25519 keypair directly into
    /// <c>indexer_identity_signing_keys</c> — what cross-node-login verification
    /// (<c>crates/server/src/cross_node_login.rs</c>) actually reads from, on any node,
    /// authoring or mirror-only alike, same as the Rust SDK's own live test seeding. No
    /// WebAuthn ceremony needed: this SDK never creates identities itself, and cross-node
    /// login's verification path only ever checks this one table, not the live-write
    /// <c>identity_signing_keys</c> a real registration would also populate. Returns the
    /// identity id, the assigned signing_key_id, and the real private key seed to sign with.</summary>
    private static async Task<(Guid IdentityId, Guid SigningKeyId, byte[] SigningKeySeed)> SeedIdentityWithSigningKeyAsync(
        NpgsqlConnection conn, string displayName)
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

        var generator = new Ed25519KeyPairGenerator();
        generator.Init(new Ed25519KeyGenerationParameters(new SecureRandom()));
        var keyPair = generator.GenerateKeyPair();
        var privateKey = (Ed25519PrivateKeyParameters)keyPair.Private;
        var publicKey = (Ed25519PublicKeyParameters)keyPair.Public;

        var signingKeyId = Guid.NewGuid();
        await using (var cmd = new NpgsqlCommand(
            "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) " +
            "VALUES ($1, $2, $3, now())", conn))
        {
            cmd.Parameters.AddWithValue(signingKeyId);
            cmd.Parameters.AddWithValue(identityId);
            cmd.Parameters.AddWithValue(publicKey.GetEncoded());
            await cmd.ExecuteNonQueryAsync();
        }

        return (identityId, signingKeyId, privateKey.GetEncoded());
    }

    /// <summary>Mirrors crates/sdk/tests/cross_node_login.rs's
    /// submit_cross_node_login_grant_resolves_directly_to_a_session — the same-device fast
    /// path, minting and submitting a real signed grant with no start/poll at all.</summary>
    [Fact]
    public async Task SubmitCrossNodeLoginGrantAsync_ResolvesDirectlyToASession()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var displayName = $"sdk-cross-node-login-csharp-{Guid.NewGuid():N}";
        var (identityId, signingKeyId, signingKeySeed) =
            await SeedIdentityWithSigningKeyAsync(conn, displayName);

        var session = await Client().SubmitCrossNodeLoginGrantAsync(identityId, signingKeyId, signingKeySeed);

        Assert.Equal(displayName, session.Profile.DisplayName);
        Assert.Equal(identityId, session.Identity.Id);
    }

    /// <summary>Mirrors crates/sdk/tests/cross_node_login.rs's
    /// submit_cross_node_login_grant_rejects_a_grant_signed_by_the_wrong_key — proves this
    /// isn't just trusting whatever identity_id/signing_key_id the caller claims.</summary>
    [Fact]
    public async Task SubmitCrossNodeLoginGrantAsync_RejectsAGrantSignedByTheWrongKey()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (identityId, signingKeyId, _realSigningKeySeed) = await SeedIdentityWithSigningKeyAsync(
            conn, $"sdk-cross-node-login-wrongkey-csharp-{Guid.NewGuid():N}");

        var generator = new Ed25519KeyPairGenerator();
        generator.Init(new Ed25519KeyGenerationParameters(new SecureRandom()));
        var impostorKey = (Ed25519PrivateKeyParameters)generator.GenerateKeyPair().Private;

        await Assert.ThrowsAsync<AvalonRequestException>(() =>
            Client().SubmitCrossNodeLoginGrantAsync(identityId, signingKeyId, impostorKey.GetEncoded()));
    }

    /// <summary>Mirrors crates/sdk/tests/cross_node_login.rs's
    /// wait_resolves_to_a_real_session_once_a_grant_is_submitted — the cross-device flow:
    /// CrossNodeLoginAsync starts a request, WaitAsync polls it, and a "simulated Hub"
    /// submits a real signed grant against its user_code shortly after, the same way
    /// crates/server/tests/cross_node_login.rs's own live test simulates approval.</summary>
    [Fact]
    public async Task CrossNodeLogin_WaitAsync_ResolvesOnceAGrantIsSubmitted()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var displayName = $"sdk-cross-node-login-wait-csharp-{Guid.NewGuid():N}";
        var (identityId, signingKeyId, signingKeySeed) =
            await SeedIdentityWithSigningKeyAsync(conn, displayName);

        var client = Client();
        var pending = await client.CrossNodeLoginAsync();
        Assert.NotEmpty(pending.UserCode);
        Assert.NotEmpty(pending.RequestingContext);
        Assert.True(pending.ExpiresIn > 0);

        var http = new HttpClient();
        var submitTask = Task.Run(async () =>
        {
            await Task.Delay(TimeSpan.FromMilliseconds(500));

            var issuedAt = DateTimeOffset.UtcNow;
            var expiresAt = issuedAt.AddSeconds(30);
            var nonce = Guid.NewGuid();
            var signingBytes = Encoding.UTF8.GetBytes(
                $"avalon:cross-node-login:v1:{identityId}:{signingKeyId}:{ServerUrl}:{ServerUrl}:{nonce}:" +
                $"{issuedAt.ToUnixTimeSeconds()}:{expiresAt.ToUnixTimeSeconds()}");
            var signer = new Ed25519Signer();
            signer.Init(true, new Ed25519PrivateKeyParameters(signingKeySeed, 0));
            signer.BlockUpdate(signingBytes, 0, signingBytes.Length);
            var signature = signer.GenerateSignature();
            var signatureHex = string.Concat(signature.Select(b => b.ToString("x2")));

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/auth/cross-node/submit");
            request.Content = new StringContent(JsonSerializer.Serialize(new
            {
                user_code = pending.UserCode,
                grant = new
                {
                    identity_id = identityId,
                    signing_key_id = signingKeyId,
                    destination_base_url = ServerUrl,
                    requesting_context = ServerUrl,
                    nonce,
                    issued_at = issuedAt,
                    expires_at = expiresAt,
                    signature = signatureHex,
                },
            }), Encoding.UTF8, "application/json");
            using var response = await http.SendAsync(request);
            response.EnsureSuccessStatusCode();
        });

        var session = await pending.WaitAsync();
        await submitTask;

        Assert.Equal(displayName, session.Profile.DisplayName);
        Assert.Equal(identityId, session.Identity.Id);
    }

    /// <summary>Mirrors crates/sdk/tests/account_session.rs's
    /// resume_account_session_signs_when_given_the_signing_key — the registration-equivalent
    /// (SQL-seeded identity + signing key, since this SDK deliberately doesn't drive a
    /// WebAuthn ceremony — see AccountSession.cs's own header comment for the scoping call) ->
    /// resume -> signature-required-action round trip issue #700 itself asks for.</summary>
    [Fact]
    public async Task AccountSession_ResumeWithSigningKeyThenCreateRole_SignsAutomaticallyAndVerifiesServerSide()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (identityId, token) = await SeedIdentitySessionAsync(conn, $"account-session-resume-csharp-{Guid.NewGuid():N}");
        var (signingKeyId, signingKey) = await SeedSigningKeyAsync(conn, identityId);

        var client = new AvalonClient(new AvalonConfig(ServerUrl!, "sdk-test"));
        var session = await client.ResumeAccountSessionWithSigningKeyAsync(token, signingKey.GetEncoded());

        Assert.Equal(identityId, session.Identity.Id);
        Assert.Equal(signingKeyId, session.SigningKeyId);

        var guild = await session.CreateGuildAsync($"Guild {Guid.NewGuid():N}".Substring(0, 20), FreshGuildTag(), "a test guild");
        Assert.Equal(identityId, guild.Owner);

        // guild.role.create is signature-required — this only succeeds if
        // AccountSession.CreateRoleAsync actually attached a valid signature the server
        // verified against signature_gate::canonical_message("guild.role.create", ...).
        var role = await session.CreateRoleAsync(guild.Id, "Quartermaster", new[] { "manage_members" }, "trusted role");
        Assert.Equal("Quartermaster", role.Name);
        Assert.Equal(new List<string> { "manage_members" }, role.Permissions);
    }

    /// <summary>Mirrors crates/sdk/tests/account_session.rs's
    /// resume_account_session_without_a_signing_key_sends_unsigned_and_is_rejected — proves
    /// a session resumed with only a bearer token (no local signing key) still works for
    /// non-signature-required actions but gets rejected server-side on a signature-required
    /// one, rather than this SDK silently fabricating a signature.</summary>
    [Fact]
    public async Task AccountSession_ResumeWithoutSigningKeyThenCreateRole_IsRejectedServerSide()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (identityId, token) = await SeedIdentitySessionAsync(conn, $"account-session-nokey-csharp-{Guid.NewGuid():N}");
        // The identity does have a registered signing key server-side — just not one this
        // session holds locally — so the server's own rejection is FRESH_SIGNATURE_REQUIRED,
        // not NO_REGISTERED_SIGNING_KEY.
        await SeedSigningKeyAsync(conn, identityId);

        var client = new AvalonClient(new AvalonConfig(ServerUrl!, "sdk-test"));
        var session = await client.ResumeAccountSessionAsync(token);
        Assert.Null(session.SigningKeyId);

        var guild = await session.CreateGuildAsync($"Guild {Guid.NewGuid():N}".Substring(0, 20), FreshGuildTag(), "an unsigned-resume test guild");

        await Assert.ThrowsAsync<AvalonRequestException>(() =>
            session.CreateRoleAsync(guild.Id, "ShouldFail", Array.Empty<string>(), ""));
    }

    /// <summary>Mirrors crates/sdk/tests/account_device_login.rs's own
    /// wait_resolves_to_a_real_account_session_once_approved — the approving
    /// side is exercised directly over HTTP against a seeded identity/session/signing key,
    /// same approach the Rust test and this file's own cross-node-login test above use.</summary>
    [Fact]
    public async Task AccountSession_StartDeviceLogin_ResolvesOnceApproved()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var displayName = $"account-device-login-csharp-{Guid.NewGuid():N}";
        var (approverId, approverToken) = await SeedIdentitySessionAsync(conn, displayName);
        var (approverKeyId, approverSigningKey) = await SeedSigningKeyAsync(conn, approverId);

        var client = new AvalonClient(new AvalonConfig(ServerUrl!, "sdk-test"));
        var pairing = await client.StartAccountDeviceLoginAsync();
        Assert.False(string.IsNullOrEmpty(pairing.UserCode));
        Assert.Contains(pairing.UserCode, pairing.VerificationUri);
        Assert.True(pairing.ExpiresIn > 0);

        var http = new HttpClient();
        var approvalTask = Task.Run(async () =>
        {
            await Task.Delay(500);
            var message = Encoding.UTF8.GetBytes($"avalon:device_pairing.approve:v1:{approverId}:{pairing.UserCode}");
            var signer = new Ed25519Signer();
            signer.Init(true, approverSigningKey);
            signer.BlockUpdate(message, 0, message.Length);
            var signature = Convert.ToBase64String(signer.GenerateSignature());

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/auth/device/approve");
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", approverToken);
            request.Content = new StringContent(JsonSerializer.Serialize(new
            {
                user_code = pairing.UserCode,
                signing_key_id = approverKeyId,
                signature,
            }), Encoding.UTF8, "application/json");
            using var response = await http.SendAsync(request);
            response.EnsureSuccessStatusCode();
        });

        var session = await pairing.WaitAsync();
        await approvalTask;

        Assert.Equal(displayName, session.Profile.DisplayName);
        // The approving device is a *different* device with its own key — this session
        // never had a WebAuthn ceremony of its own, so it holds no local signing key.
        Assert.Null(session.SigningKeyId);
    }

    /// <summary>Mirrors crates/sdk/tests/account_device_login.rs's own
    /// wait_returns_a_typed_error_when_denied.</summary>
    [Fact]
    public async Task AccountSession_StartDeviceLogin_ThrowsWhenDenied()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (_, approverToken) =
            await SeedIdentitySessionAsync(conn, $"account-device-login-deny-csharp-{Guid.NewGuid():N}");

        var client = new AvalonClient(new AvalonConfig(ServerUrl!, "sdk-test"));
        var pairing = await client.StartAccountDeviceLoginAsync();

        var http = new HttpClient();
        var denialTask = Task.Run(async () =>
        {
            await Task.Delay(500);
            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/auth/device/deny");
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", approverToken);
            request.Content = new StringContent(JsonSerializer.Serialize(new { user_code = pairing.UserCode }), Encoding.UTF8, "application/json");
            using var response = await http.SendAsync(request);
            response.EnsureSuccessStatusCode();
        });

        await Assert.ThrowsAsync<AccountDeviceLoginDeniedException>(() => pairing.WaitAsync());
        await denialTask;
    }

    // --- Issue #741/#744-#749: register-integrator -> add-issuer-key -> whoami,
    // achievement-definition create -> issue -> read, and publish-recognition ->
    // list-recognitions -> revoke round trips, entirely through the SDK's own
    // new call sites (no raw HTTP needed for any of these three, unlike the
    // older achievement live tests above which predate this SDK having its own
    // RegisterIntegratorAsync/CreateAchievementDefinitionAsync).

    [Fact]
    public async Task RegisterIntegratorThenAddIssuerKeyThenWhoami_RoundTripsThroughTheSdk()
    {
        if (ServerUrl is null) return;

        var suffix = Guid.NewGuid().ToString("N").Substring(0, 10);
        var client = Client();

        var random = new SecureRandom();
        var keyGen = new Ed25519KeyPairGenerator();
        keyGen.Init(new Ed25519KeyGenerationParameters(random));
        var rootKeyPair = keyGen.GenerateKeyPair();
        var rootPrivateKey = (Ed25519PrivateKeyParameters)rootKeyPair.Private;
        var rootPublicKey = (Ed25519PublicKeyParameters)rootKeyPair.Public;

        var integrator = await client.RegisterIntegratorAsync(
            $"sdk-reg-{suffix}",
            $"SDK Registration Test {suffix}",
            "Test Studio",
            "ed25519",
            Convert.ToBase64String(rootPublicKey.GetEncoded()));

        var rootSession = new AvalonClient(new AvalonConfig(
            ServerUrl!, integrator.Credential.KeyId, integrator.Slug, rootPrivateKey.GetEncoded()));
        // No identity token is actually needed for challenge-authenticated integrator calls
        // (see Session.AttachIntegratorAuthAsync) — but Session still requires a real
        // AuthenticateAsync to exist, so authenticate against a throwaway identity via a
        // direct DB seed would be needed for full Session construction. Instead, exercise
        // AddIssuerKeyAsync/IntegratorWhoamiAsync directly against the registered root key
        // through Session.ForTesting, which needs no identity token at all for these two
        // challenge-only calls.
        var session = Session.ForTesting(
            Array.Empty<string>(),
            new HttpClient(),
            serverUrl: ServerUrl!,
            integratorKeyId: integrator.Credential.KeyId,
            integratorSlug: integrator.Slug,
            signingKey: rootPrivateKey.GetEncoded());

        keyGen.Init(new Ed25519KeyGenerationParameters(random));
        var operationalKeyPair = keyGen.GenerateKeyPair();
        var operationalPublicKey = (Ed25519PublicKeyParameters)operationalKeyPair.Public;
        var addedKey = await session.AddIssuerKeyAsync(
            "ed25519", Convert.ToBase64String(operationalPublicKey.GetEncoded()), "operational");
        Assert.Equal("operational", addedKey.Role);

        var whoami = await session.IntegratorWhoamiAsync();
        Assert.Equal(integrator.Id, whoami);

        var keys = await session.ListIssuerKeysAsync(integrator.Slug);
        Assert.Contains(keys, k => k.KeyId == addedKey.KeyId);
    }

    [Fact]
    public async Task CreateAchievementDefinitionThenIssueThenReadItBack_RoundTripsThroughTheSdk()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (identityId, token) = await SeedIdentitySessionAsync(conn, $"sdk-def-csharp-{Guid.NewGuid():N}");
        var (signingKeyId, signingKey) = await SeedSigningKeyAsync(conn, identityId);

        var http = new HttpClient();
        var integrator = await RegisterIntegratorAsync(http, ServerUrl!);
        await ConnectIntegratorAsync(http, ServerUrl!, integrator.Slug, token, signingKeyId, signingKey, "achievements.issue", "achievements.read");

        var client = new AvalonClient(new AvalonConfig(
            ServerUrl!, integrator.KeyId, integrator.Slug, integrator.SigningKeySeed));
        var session = await client.AuthenticateAsync(token);

        var key = $"sdk_def_{Guid.NewGuid():N}".Substring(0, 20);
        var definition = await session.CreateAchievementDefinitionAsync(key, "SDK Defined", "Defined via the SDK itself");
        Assert.Equal(key, definition.Key);

        var attestationId = await session.IssueAchievementAsync(key);
        var attestation = await session.GetAttestationAsync(attestationId);

        Assert.Equal(attestationId, attestation.Id);
        Assert.True(attestation.Authenticity.IsAuthentic);
        Assert.True(attestation.Validity.IsValid);

        var updated = await session.UpdateAchievementDefinitionAsync(key, description: "Updated via the SDK");
        Assert.Equal("Updated via the SDK", updated.Description);

        var definitions = await session.ListAchievementDefinitionsAsync(integrator.Slug);
        Assert.Contains(definitions, d => d.Key == key);
    }

    [Fact]
    public async Task PublishRecognitionThenListThenRevoke_RoundTripsThroughTheSdk()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (identityId, token) = await SeedIdentitySessionAsync(conn, $"sdk-recognition-csharp-{Guid.NewGuid():N}");
        var (signingKeyId, signingKey) = await SeedSigningKeyAsync(conn, identityId);

        var http = new HttpClient();
        var recognizer = await RegisterIntegratorAsync(http, ServerUrl!);
        var recognized = await RegisterIntegratorAsync(http, ServerUrl!);
        await ConnectIntegratorAsync(http, ServerUrl!, recognizer.Slug, token, signingKeyId, signingKey);

        var client = new AvalonClient(new AvalonConfig(
            ServerUrl!, recognizer.KeyId, recognizer.Slug, recognizer.SigningKeySeed));
        var session = await client.AuthenticateAsync(token);

        var published = await session.PublishRecognitionAsync(recognized.Slug, new[] { "achievements" });
        Assert.Equal(recognized.Slug, published.RecognizedSlug);

        var recognitions = await session.ListRecognitionsAsync(recognizer.Slug);
        Assert.Contains(recognitions, r => r.RecognizedSlug == recognized.Slug);

        var recognizedBy = await session.ListRecognizedByAsync(recognized.Slug);
        Assert.Contains(recognizedBy, r => r.RecognizerSlug == recognizer.Slug);

        var revoked = await session.RevokeRecognitionAsync(recognized.Slug);
        Assert.True(revoked);

        var afterRevoke = await session.ListRecognitionsAsync(recognizer.Slug);
        Assert.DoesNotContain(afterRevoke, r => r.RecognizedSlug == recognized.Slug);
    }

    [Fact]
    public async Task GetIdentityRecoveryStatusAsync_NoActiveRecovery_ReturnsNullAgainstARealServer()
    {
        if (ServerUrl is null || DatabaseUrl is null) return;

        await using var conn = new NpgsqlConnection(DatabaseUrl);
        await conn.OpenAsync();
        var (identityId, _) = await SeedIdentitySessionAsync(conn, $"sdk-recovery-status-csharp-{Guid.NewGuid():N}");

        var client = Client();
        var status = await client.GetIdentityRecoveryStatusAsync(identityId);

        Assert.Null(status);
    }
}
