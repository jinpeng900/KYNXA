using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Services;

static void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

static async Task Reject(Func<Task> action, string message)
{
    try { await action(); }
    catch (InvalidOperationException) { return; }
    throw new InvalidOperationException(message);
}

using var reservation = new TcpListener(IPAddress.Loopback, 0);
reservation.Start();
int port = ((IPEndPoint)reservation.LocalEndpoint).Port;
reservation.Stop();
using var listener = new HttpListener();
listener.Prefixes.Add($"http://127.0.0.1:{port}/");
listener.Start();
using var listenerShutdown = new CancellationTokenSource();
var json = new JsonSerializerOptions(JsonSerializerDefaults.Web);
var requests = new List<(string Path, int Tokens)>();
int requestCount = 0;
Exception? serverFailure = null;
var server = Task.Run(async () =>
{
    try
    {
        while (listener.IsListening)
        {
            var context = await listener.GetContextAsync();
            Interlocked.Increment(ref requestCount);
            object? result;
            try
            {
                string path = context.Request.Url!.AbsolutePath;
                if (path == "/health")
                    result = new { service = "kynxa-model-gateway", status = "ok", storageProtocol = 1,
                        conversationProtocol = 1, memoryProtocol = 1, contextProtocol = 1, dataLayoutVersion = 1 };
                else if (path == "/api/models" && context.Request.HttpMethod == "GET")
                {
                    // Older gateways do not include contextWindowTokens. Reading
                    // one must not change the saved connection or default to zero.
                    result = new { providers = new[] { new { providerId = "fixture-legacy", displayName = "Legacy",
                        baseUrl = "http://127.0.0.1:8080/v1", models = new[] { "fixture-model" },
                        hasApiKey = false, protocol = "openai-completions" } } };
                }
                else
                {
                    using var reader = new StreamReader(context.Request.InputStream, Encoding.UTF8);
                    string body = await reader.ReadToEndAsync();
                    using var document = JsonDocument.Parse(body);
                    Check(document.RootElement.TryGetProperty("contextWindowTokens", out var value), "context field missing on outgoing request");
                    Check(value.TryGetInt32(out int tokens), "context field is not an integer");
                    var connection = JsonSerializer.Deserialize<ModelConnection>(body, json)!;
                    requests.Add((path, tokens));
                    result = path == "/api/models/test"
                        ? new { ok = true, latencyMs = 1, models = new[] { "fixture-model" } }
                        : new { provider = new ModelProvider(connection.ProviderId, connection.DisplayName, connection.BaseUrl,
                            connection.Models, false, connection.Protocol, connection.ContextWindowTokens) };
                }
            }
            catch (Exception error)
            {
                serverFailure = error;
                context.Response.StatusCode = 500;
                result = new { error = error.Message };
            }
            byte[] response = JsonSerializer.SerializeToUtf8Bytes(result, json);
            context.Response.ContentType = "application/json";
            context.Response.ContentLength64 = response.Length;
            await context.Response.OutputStream.WriteAsync(response);
            context.Response.Close();
        }
    }
    catch (Exception error) when (listenerShutdown.IsCancellationRequested &&
        error is HttpListenerException or ObjectDisposedException or InvalidOperationException) { }
});

string? previousAddress = Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL");
Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{port}");
try
{
    using var client = new ModelApiClient();
    var providers = await client.ListAsync();
    Check(providers.Single().ContextWindowTokens == 8192, "legacy response did not use the conservative default");
    var connection = new ModelConnection("fixture-local", "Synthetic model", "http://127.0.0.1:8080/v1", ["fixture-model"]);
    var saved = await client.SaveAsync(connection);
    Check(saved.ContextWindowTokens == 8192 && requests[^1].Tokens == 8192, "new connection default was not sent and preserved");
    var oneMillion = connection with { ContextWindowTokens = 1_000_000 };
    saved = await client.SaveAsync(oneMillion);
    Check(saved.ContextWindowTokens == 1_000_000 && requests[^1].Tokens == 1_000_000, "one-million context did not round-trip");
    var test = await client.TestAsync(oneMillion);
    Check(test.Ok && requests[^1] == ("/api/models/test", 1_000_000), "connection probe dropped its context configuration");
    foreach (int tokens in new[] { 2048, 32768, 131072, 262144, 123456, 2000000 })
    {
        saved = await client.SaveAsync(connection with { ContextWindowTokens = tokens });
        Check(saved.ContextWindowTokens == tokens, $"saved configured window {tokens} changed");
    }
    int before = Volatile.Read(ref requestCount);
    foreach (int invalid in new[] { 0, -1, 2047, 2000001, int.MaxValue })
    {
        await Reject(() => client.SaveAsync(connection with { ContextWindowTokens = invalid }), "invalid context saved");
        await Reject(() => client.TestAsync(connection with { ContextWindowTokens = invalid }), "invalid context tested");
    }
    Check(Volatile.Read(ref requestCount) == before, "invalid windows reached the gateway");
    Check(serverFailure is null, $"fake gateway failed: {serverFailure}");
    Console.WriteLine("PASS: old-response 8192 default, 1M Save/Test transport, preset/custom/boundary round trips, and invalid contexts rejected before network access.");
}
finally
{
    Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", previousAddress);
    listenerShutdown.Cancel();
    listener.Stop();
    await server;
}
