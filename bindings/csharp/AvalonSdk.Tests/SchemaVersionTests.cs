using System;
using System.IO;
using System.Text.Json;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

// Issue #735/#725: OpenApiSchemaVersion is generated straight from
// docs/generated/openapi.json's own info.version by bindings/csharp/codegen,
// so it can't drift by construction — this exists to catch a future change
// to that generation step breaking the link, not routine drift.
public class SchemaVersionTests
{
    [Fact]
    public void OpenApiSchemaVersion_MatchesOpenApiJson()
    {
        var dir = AppContext.BaseDirectory;
        var current = new DirectoryInfo(dir);
        while (current is not null && !File.Exists(Path.Combine(current.FullName, "docs", "generated", "openapi.json")))
        {
            current = current.Parent;
        }
        Assert.NotNull(current);
        var schemaPath = Path.Combine(current!.FullName, "docs", "generated", "openapi.json");
        using var doc = JsonDocument.Parse(File.ReadAllText(schemaPath));
        var expected = doc.RootElement.GetProperty("info").GetProperty("version").GetString();

        Assert.Equal(AvalonClient.OpenApiSchemaVersion, expected);
    }
}
