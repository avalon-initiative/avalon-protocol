using System;
using System.Linq;
using System.Net;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class AchievementsTests
{
    private static readonly byte[] TestSigningKey = Enumerable.Range(0, 32).Select(i => (byte)i).ToArray();

    [Fact]
    public async Task GetAchievementsAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.GetAchievementsAsync());
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task IssueAchievementAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.IssueAchievementAsync("dragon_slayer"));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task IssueAchievementAsync_WithoutConfiguredIssuerCredentials_ThrowsBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(new[] { "achievements.issue" }, handler.ToHttpClient());

        await Assert.ThrowsAsync<MissingIssuerCredentialsException>(() => session.IssueAchievementAsync("dragon_slayer"));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task GetAchievementsAsync_PopulatesAuthenticityAndValidityAsSeparateFields()
    {
        var subject = Guid.NewGuid();
        var attestationId = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        {
            "achievements": [
                {
                    "id": "{{attestationId}}",
                    "issuer": "game:dragons-inc",
                    "subject": "{{subject}}",
                    "achievement": "game:dragons-inc:achievement:dragon_slayer",
                    "issued_at": "2026-01-01T00:00:00Z",
                    "authenticity": { "status": "authentic", "key_id": "primary" },
                    "validity": { "status": "valid" },
                    "history": [
                        { "event": "issued", "at": "2026-01-01T00:00:00Z", "reason_code": null, "reason": null }
                    ]
                }
            ],
            "next_cursor": null
        }
        """);
        var session = Session.ForTesting(new[] { "achievements.read" }, handler.ToHttpClient());

        var history = await session.GetAchievementsAsync();

        var attestation = Assert.Single(history);
        Assert.Equal(attestationId, attestation.Id);
        Assert.True(attestation.Authenticity.IsAuthentic);
        Assert.True(attestation.Validity.IsValid);
        Assert.Single(attestation.History);
        Assert.Equal("issued", attestation.History[0].Event);
        Assert.Equal(HttpMethod.Get, handler.Requests[0].Method);
        Assert.Contains("/me/achievements", handler.Requests[0].Url);
    }

    [Fact]
    public async Task GetAchievementsAsync_RevokedAttestation_IsInvalidNotAuthenticityFailure()
    {
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        {
            "achievements": [
                {
                    "id": "{{Guid.NewGuid()}}",
                    "issuer": "game:dragons-inc",
                    "subject": "{{Guid.NewGuid()}}",
                    "achievement": "game:dragons-inc:achievement:dragon_slayer",
                    "issued_at": "2026-01-01T00:00:00Z",
                    "authenticity": { "status": "authentic", "key_id": "primary" },
                    "validity": { "status": "invalid", "reason": "revoked" },
                    "history": [
                        { "event": "issued", "at": "2026-01-01T00:00:00Z" },
                        { "event": "revoked", "at": "2026-01-02T00:00:00Z", "reason_code": "issuer_error", "reason": "issued by mistake" }
                    ]
                }
            ]
        }
        """);
        var session = Session.ForTesting(new[] { "achievements.read" }, handler.ToHttpClient());

        var attestation = Assert.Single(await session.GetAchievementsAsync());

        Assert.True(attestation.Authenticity.IsAuthentic);
        Assert.False(attestation.Validity.IsValid);
        Assert.Equal(2, attestation.History.Count);
    }

    [Fact]
    public async Task IssueAchievementAsync_SendsAChallengeThenAnIssueRequestAndReturnsTheNewId()
    {
        var attestationId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "chal-1", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""{ "id": "{{attestationId}}" }""");
        var session = Session.ForTesting(
            new[] { "achievements.issue" },
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var id = await session.IssueAchievementAsync("dragon_slayer");

        Assert.Equal(attestationId, id);
        Assert.Equal(2, handler.Requests.Count);
        Assert.Contains("/integrations/dragons-inc/challenge", handler.Requests[0].Url);
        Assert.Contains("/integrations/dragons-inc/achievements/dragon_slayer/issue", handler.Requests[1].Url);
    }

    [Fact]
    public async Task IssueAchievementAsync_NonSuccessResponse_ThrowsAvalonRequestException()
    {
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "chal-1", "nonce": "{{nonce}}" }""")
            .Enqueue(HttpStatusCode.Forbidden, """{ "error": "forbidden", "code": "FORBIDDEN" }""");
        var session = Session.ForTesting(
            new[] { "achievements.issue" },
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        await Assert.ThrowsAsync<AvalonRequestException>(() => session.IssueAchievementAsync("dragon_slayer"));
    }
}
