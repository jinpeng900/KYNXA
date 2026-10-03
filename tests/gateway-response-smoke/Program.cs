using System.Net;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
    checks++;
}

HttpResponseMessage Response(HttpStatusCode status, string body) =>
    new(status) { Content = new StringContent(body, Encoding.UTF8, "application/json") };

async Task<GatewayApiException> Failure(HttpResponseMessage response, string? conflictMessage = null)
{
    try
    {
        await GatewayResponseReader.EnsureSuccessAsync(response, "HTTP {0}", CancellationToken.None, conflictMessage);
        throw new Exception("HTTP failure was accepted.");
    }
    catch (GatewayApiException error) { return error; }
}

using (var response = Response(HttpStatusCode.BadRequest, "{\"error\":\"配置无效\",\"code\":\"INVALID_CONNECTION\"}"))
{
    var error = await Failure(response);
    Check(error is InvalidOperationException && error.Message == "配置无效", "Existing error handling or gateway message changed.");
    Check(error.StatusCode == HttpStatusCode.BadRequest && error.ErrorCode == "INVALID_CONNECTION", "HTTP status or machine code was discarded.");
}

foreach (string body in new[] { "<html>private upstream response</html>", "{", "null", "[]", "{\"error\":42,\"code\":false}" })
{
    using var response = Response(HttpStatusCode.BadGateway, body);
    var error = await Failure(response);
    Check(error.Message == "HTTP 502" && error.StatusCode == HttpStatusCode.BadGateway, "Malformed error did not retain the status fallback.");
    Check(error.ErrorCode is null && !error.Message.Contains("private upstream"), "Malformed code or raw upstream content escaped.");
}

using (var response = Response(HttpStatusCode.ServiceUnavailable, "{\"code\":\"STORAGE_MAINTENANCE\"}"))
{
    var error = await Failure(response);
    Check(error.Message == "HTTP 503" && error.ErrorCode == "STORAGE_MAINTENANCE", "Code-only failure lost its identity.");
}

using (var response = Response(HttpStatusCode.Conflict, "not JSON"))
{
    var error = await Failure(response, "Reload the catalog.");
    Check(error.Message == "Reload the catalog." && error.StatusCode == HttpStatusCode.Conflict, "Catalog recovery instruction changed.");
}

using (var response = Response(HttpStatusCode.Conflict, "{\"error\":\"stale revision\",\"code\":\"REVISION_CONFLICT\"}"))
{
    var error = await Failure(response);
    Check(error.Message == "stale revision" && error.ErrorCode == "REVISION_CONFLICT", "Non-catalog conflicts were incorrectly overridden.");
}

using (var response = Response(HttpStatusCode.OK, "{\"displayName\":\"旧接口\"}"))
{
    var result = await GatewayResponseReader.ReadAsync<FixtureResponse>(response, "empty", "HTTP {0}", CancellationToken.None);
    Check(result.DisplayName == "旧接口" && result.Tokens == 8192, "Default web JSON or missing-field compatibility changed.");
}

using (var response = Response(HttpStatusCode.OK, "{\"DisplayName\":\"目录格式\",\"Tokens\":1000000}"))
{
    var result = await GatewayResponseReader.ReadAsync<FixtureResponse>(response, "empty", "HTTP {0}", CancellationToken.None,
        new JsonSerializerOptions { PropertyNameCaseInsensitive = false });
    Check(result.DisplayName == "目录格式" && result.Tokens == 1000000, "Caller JSON options were ignored.");
}

using (var response = Response(HttpStatusCode.OK, "null"))
{
    try
    {
        await GatewayResponseReader.ReadAsync<FixtureResponse>(response, "Empty gateway response.", "HTTP {0}", CancellationToken.None);
        throw new Exception("Empty success accepted.");
    }
    catch (InvalidDataException error) { Check(error.Message == "Empty gateway response.", "Empty-response instruction changed."); }
}

using (var response = Response(HttpStatusCode.OK, "not JSON"))
{
    try
    {
        await GatewayResponseReader.ReadAsync<FixtureResponse>(response, "empty", "HTTP {0}", CancellationToken.None);
        throw new Exception("Invalid success accepted.");
    }
    catch (JsonException) { Check(true, "Invalid success retains its parse failure."); }
}

using (var cancelled = new CancellationTokenSource())
using (var response = Response(HttpStatusCode.BadRequest, "{\"error\":\"failure\"}"))
{
    cancelled.Cancel();
    try
    {
        await GatewayResponseReader.EnsureSuccessAsync(response, "HTTP {0}", cancelled.Token);
        throw new Exception("Cancellation ignored.");
    }
    catch (OperationCanceledException) { Check(true, "Cancellation is not converted into an API failure."); }
}

var content = new TrackingContent("{\"displayName\":\"owned by caller\"}");
using (var response = new HttpResponseMessage(HttpStatusCode.OK) { Content = content })
{
    await GatewayResponseReader.EnsureSuccessAsync(response, "HTTP {0}", CancellationToken.None);
    Check(!content.IsDisposed, "Response validation closed a successful stream.");
    var result = await GatewayResponseReader.ReadAsync<FixtureResponse>(response, "empty", "HTTP {0}", CancellationToken.None);
    Check(result.DisplayName == "owned by caller" && !content.IsDisposed, "Reader disposed the caller's response.");
}
Check(content.IsDisposed, "Response ownership was not released by the caller.");
Console.WriteLine($"PASS: {checks} gateway response checks covering JSON compatibility, HTTP/code identity, error fallback, conflicts, cancellation and response ownership.");

sealed record FixtureResponse(string DisplayName, int Tokens = 8192);

sealed class TrackingContent(string body) : HttpContent
{
    private readonly byte[] _body = Encoding.UTF8.GetBytes(body);
    public bool IsDisposed { get; private set; }

    protected override Task SerializeToStreamAsync(Stream stream, TransportContext? context) => stream.WriteAsync(_body).AsTask();
    protected override bool TryComputeLength(out long length) { length = _body.Length; return true; }
    protected override void Dispose(bool disposing) { IsDisposed = true; base.Dispose(disposing); }
}
