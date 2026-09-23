using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk.Tests;

/// <summary>
/// A stubbed HttpMessageHandler that returns a canned JSON response for each request it sees,
/// in call order — the same shape used for the SDK's HTTP unit tests. Each call is also
/// recorded so a test can assert on the method/URL/headers a Session method actually sent.
/// </summary>
internal sealed class StubHttpMessageHandler : HttpMessageHandler
{
    // Body is captured synchronously here (read before SendAsync returns) so AccountSession's
    // signed-request tests can assert on the exact signing_key_id/signature the
    // request body carried, not just its method/URL/auth header.
    public sealed record RecordedRequest(HttpMethod Method, string Url, string? AuthorizationToken, string? Body);

    private readonly Queue<(HttpStatusCode Status, string Body)> _responses = new();
    public List<RecordedRequest> Requests { get; } = new();

    public StubHttpMessageHandler Enqueue(HttpStatusCode status, string body)
    {
        _responses.Enqueue((status, body));
        return this;
    }

    public StubHttpMessageHandler Enqueue(string jsonBody) => Enqueue(HttpStatusCode.OK, jsonBody);

    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        var requestBody = request.Content?.ReadAsStringAsync(cancellationToken).GetAwaiter().GetResult();
        Requests.Add(new RecordedRequest(
            request.Method,
            request.RequestUri!.ToString(),
            request.Headers.Authorization?.Parameter,
            requestBody));

        if (_responses.Count == 0)
        {
            throw new InvalidOperationException("StubHttpMessageHandler received a request with no queued response: " + request.RequestUri);
        }

        var (status, body) = _responses.Dequeue();
        var response = new HttpResponseMessage(status)
        {
            Content = new StringContent(body, Encoding.UTF8, "application/json"),
        };
        return Task.FromResult(response);
    }

    public HttpClient ToHttpClient() => new(this);
}
