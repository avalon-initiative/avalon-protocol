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
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
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
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue(HttpStatusCode.Forbidden, """{ "error": "forbidden", "code": "FORBIDDEN" }""");
        var session = Session.ForTesting(
            new[] { "achievements.issue" },
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        await Assert.ThrowsAsync<AvalonRequestException>(() => session.IssueAchievementAsync("dragon_slayer"));
    }

    // --- Issue #744: attestation read/revoke, definition CRUD, bulk issuance, milestones ---

    [Fact]
    public async Task GetAttestationAsync_IsPublic_NoAuthorizationHeaderSent()
    {
        var attestationId = Guid.NewGuid();
        var subject = Guid.NewGuid();
        var handler = new StubHttpMessageHandler().Enqueue($$"""
        {
            "id": "{{attestationId}}",
            "issuer": "game:dragons-inc",
            "subject": "{{subject}}",
            "achievement": "game:dragons-inc:achievement:dragon_slayer",
            "issued_at": "2026-01-01T00:00:00Z",
            "proof": { "key_id": "{{Guid.NewGuid()}}", "algorithm": "ed25519", "bytes": "AA==" },
            "authenticity": { "status": "authentic", "key_id": "primary" },
            "validity": { "status": "valid" },
            "history": [ { "event": "issued", "at": "2026-01-01T00:00:00Z" } ]
        }
        """);
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var attestation = await session.GetAttestationAsync(attestationId);

        Assert.Equal(attestationId, attestation.Id);
        Assert.True(attestation.Authenticity.IsAuthentic);
        Assert.Null(handler.Requests[0].AuthorizationToken);
    }

    [Fact]
    public async Task RevokeAttestationAsync_WithoutConfiguredIssuerCredentials_ThrowsBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<MissingIssuerCredentialsException>(
            () => session.RevokeAttestationAsync(Guid.NewGuid(), "achievement", "game:dragons-inc", "issuer_error", "mistake"));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task RevokeAttestationAsync_SendsAChallengeThenARevokeRequest()
    {
        var attestationId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""{ "attestation_id": "{{attestationId}}", "revoked_at": "2026-01-02T00:00:00Z", "reason_code": "issuer_error", "reason": "mistake" }""");
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var revocation = await session.RevokeAttestationAsync(attestationId, "achievement", "game:dragons-inc", "issuer_error", "mistake");

        Assert.Equal(attestationId, revocation.AttestationId);
        Assert.Equal(2, handler.Requests.Count);
        Assert.Contains($"/attestations/{attestationId}/revoke", handler.Requests[1].Url);
    }

    [Fact]
    public async Task CreateAchievementDefinitionAsync_WithoutConfiguredSlug_ThrowsBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<MissingIssuerCredentialsException>(
            () => session.CreateAchievementDefinitionAsync("dragon_slayer", "Dragon Slayer", "Slew a dragon"));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task CreateAchievementDefinitionAsync_SendsAChallengeThenACreateRequest()
    {
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""
            {
                "id": "game:dragons-inc:achievement:dragon_slayer", "integrator_id": "{{Guid.NewGuid()}}",
                "key": "dragon_slayer", "name": "Dragon Slayer", "description": "Slew a dragon",
                "schema": null, "icon": "trophy", "icon_url": null, "version": 1,
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z", "retired": false, "retired_at": null
            }
            """);
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var definition = await session.CreateAchievementDefinitionAsync("dragon_slayer", "Dragon Slayer", "Slew a dragon");

        Assert.Equal("dragon_slayer", definition.Key);
        Assert.Equal(HttpMethod.Post, handler.Requests[1].Method);
        Assert.Contains("/integrations/dragons-inc/achievements", handler.Requests[1].Url);
    }

    [Fact]
    public async Task ListAchievementDefinitionsAsync_IsPublic_NoAuthorizationHeaderRequired()
    {
        var handler = new StubHttpMessageHandler().Enqueue("[]");
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var definitions = await session.ListAchievementDefinitionsAsync("dragons-inc");

        Assert.Empty(definitions);
        Assert.Contains("/integrations/dragons-inc/achievements", handler.Requests[0].Url);
    }

    [Fact]
    public async Task BulkIssueAchievementsAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(
            () => session.BulkIssueAchievementsAsync(new[] { new Session.BulkClaim("dragon_slayer") }));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task BulkIssueAchievementsAsync_ReportsPerClaimOutcomes()
    {
        var attestationId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""
            {
                "results": [
                    { "status": "issued", "key": "dragon_slayer", "attestation": { "id": "{{attestationId}}", "issuer": "game:dragons-inc", "subject": "{{Guid.NewGuid()}}", "achievement": "game:dragons-inc:achievement:dragon_slayer", "issued_at": "2026-01-01T00:00:00Z", "proof": { "key_id": "{{Guid.NewGuid()}}", "algorithm": "ed25519", "bytes": "AA==" } } },
                    { "status": "failed", "key": "unknown_key", "code": "ACHIEVEMENT_DEFINITION_NOT_FOUND", "error": "achievement definition not found" }
                ]
            }
            """);
        var session = Session.ForTesting(
            new[] { "achievements.issue" },
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var results = await session.BulkIssueAchievementsAsync(new[]
        {
            new Session.BulkClaim("dragon_slayer"),
            new Session.BulkClaim("unknown_key"),
        });

        Assert.Equal(2, results.Count);
        Assert.True(results[0].IsIssued);
        Assert.Equal(attestationId, results[0].Attestation!.Id);
        Assert.False(results[1].IsIssued);
        Assert.Equal("ACHIEVEMENT_DEFINITION_NOT_FOUND", results[1].Code);
        Assert.Contains("/integrations/dragons-inc/achievements/bulk-issue", handler.Requests[1].Url);
    }

    [Fact]
    public async Task IssueMilestoneAsync_WithoutGrant_IsRejectedBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<CapabilityNotGrantedException>(() => session.IssueMilestoneAsync("onboarded", "app"));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task IssueMilestoneAsync_SendsAChallengeThenAnIssueRequestUnderTheMilestonesRoute()
    {
        var attestationId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""{ "id": "{{attestationId}}" }""");
        var session = Session.ForTesting(
            new[] { "milestones.issue" },
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "wallet-app",
            signingKey: TestSigningKey);

        var id = await session.IssueMilestoneAsync("onboarded", "app");

        Assert.Equal(attestationId, id);
        Assert.Contains("/integrations/wallet-app/milestones/onboarded/issue", handler.Requests[1].Url);
    }
}
