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
}
