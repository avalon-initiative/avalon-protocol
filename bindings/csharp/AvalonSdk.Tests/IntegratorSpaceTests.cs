using System;
using System.Linq;
using System.Threading.Tasks;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class IntegratorSpaceTests
{
    private static readonly byte[] TestSigningKey = Enumerable.Range(0, 32).Select(i => (byte)i).ToArray();

    [Fact]
    public async Task PublishSchemaVersionAsync_SendsAChallengeThenAPublishRequest()
    {
        var integratorId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""
            {
                "id": "game:dragons-inc:schema:1", "integrator_id": "{{integratorId}}", "version": 1,
                "proto_source": "message Character { string name = 1; }", "published_at": "2026-01-01T00:00:00Z",
                "superseded_by": null, "default_visibility": "public", "field_visibility": {}
            }
            """);
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var version = await session.PublishSchemaVersionAsync("message Character { string name = 1; }");

        Assert.Equal(1, version.Version);
        Assert.Contains("/integrations/dragons-inc/schemas", handler.Requests[1].Url);
    }

    [Fact]
    public async Task PublishSchemaVersionAsync_WithoutConfiguredSlug_ThrowsBeforeAnyRequest()
    {
        var handler = new StubHttpMessageHandler();
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        await Assert.ThrowsAsync<MissingIssuerCredentialsException>(() => session.PublishSchemaVersionAsync("message X {}"));
        Assert.Empty(handler.Requests);
    }

    [Fact]
    public async Task ListSchemaVersionsAsync_IsPublic_NoAuthorizationHeaderSent()
    {
        var handler = new StubHttpMessageHandler().Enqueue("[]");
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var versions = await session.ListSchemaVersionsAsync("dragons-inc");

        Assert.Empty(versions);
        Assert.Equal(HttpMethod.Get, handler.Requests[0].Method);
    }

    [Fact]
    public async Task PublishMappingAsync_SendsAChallengeThenAPublishRequest()
    {
        var integratorId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""
            {
                "id": "game:dragons-inc:schema_mapping:1", "integrator_id": "{{integratorId}}",
                "from_schema_id": "game:dragons-inc:schema:1", "to_schema_id": "game:dragons-inc:schema:2",
                "description": "renamed field", "field_correspondence": { "old": "new" }, "published_at": "2026-01-01T00:00:00Z"
            }
            """);
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var mapping = await session.PublishMappingAsync("game:dragons-inc:schema:1", "game:dragons-inc:schema:2");

        Assert.Equal("game:dragons-inc:schema:1", mapping.FromSchemaId);
        Assert.Contains("/integrations/dragons-inc/mappings", handler.Requests[1].Url);
    }

    [Fact]
    public async Task PublishInstanceAsync_SendsAChallengeThenAPublishRequest()
    {
        var subject = Guid.NewGuid();
        var integratorId = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue($$"""
            {
                "id": "{{Guid.NewGuid()}}", "schema_id": "game:dragons-inc:schema:1", "integrator_id": "{{integratorId}}",
                "subject": "{{subject}}", "instance": { "level": 5 }, "published_at": "2026-01-01T00:00:00Z", "superseded_by": null
            }
            """);
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        var instance = await session.PublishInstanceAsync(1, subject, new { level = 5 });

        Assert.Equal(subject, instance.Subject);
        Assert.Contains("/integrations/dragons-inc/schemas/1/data", handler.Requests[1].Url);
    }

    [Fact]
    public async Task DeleteInstanceAsync_SendsAChallengeThenADeleteRequest()
    {
        var subject = Guid.NewGuid();
        var nonce = Convert.ToBase64String(new byte[] { 1, 2, 3, 4 });
        var handler = new StubHttpMessageHandler()
            .Enqueue($$"""{ "challenge_id": "11111111-1111-1111-1111-111111111111", "nonce": "{{nonce}}" }""")
            .Enqueue("{}");
        var session = Session.ForTesting(
            Array.Empty<string>(),
            handler.ToHttpClient(),
            integratorKeyId: Guid.NewGuid().ToString(),
            integratorSlug: "dragons-inc",
            signingKey: TestSigningKey);

        await session.DeleteInstanceAsync(1, subject);

        Assert.Equal(HttpMethod.Delete, handler.Requests[1].Method);
        Assert.Contains($"/integrations/dragons-inc/schemas/1/data/{subject}", handler.Requests[1].Url);
    }

    [Fact]
    public async Task GetIdentityIntegratorDataAsync_IsPublic_NoAuthorizationHeaderSent()
    {
        var handler = new StubHttpMessageHandler().Enqueue("[]");
        var session = Session.ForTesting(Array.Empty<string>(), handler.ToHttpClient());

        var data = await session.GetIdentityIntegratorDataAsync(Guid.NewGuid());

        Assert.Empty(data);
        Assert.Equal(HttpMethod.Get, handler.Requests[0].Method);
    }
}
