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
var requests = new List<(string Path, int ContextTokens, int OutputTokens)>();
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
                        conversationProtocol = 1, memoryProtocol = 1, contextProtocol = 3, agentProtocol = 5, officialToolsProtocol = 2, hostTerminalProtocol = 3, browserAutomationProtocol = 2, extensionStorageProtocol = 1, toolStreamProtocol = 3, dataLayoutVersion = 1 };
                else if (path == "/api/models" && context.Request.HttpMethod == "GET")
                {
                    // Missing limits use their independent defaults. Explicit limits
                    // from an existing connection must survive loading unchanged.
                    result = new { providers = new object[] { new { providerId = "fixture-legacy", displayName = "Legacy",
                        baseUrl = "http://127.0.0.1:8080/v1", models = new[] { "fixture-model" },
                        hasApiKey = false, protocol = "openai-completions" }, new {
                        providerId = "fixture-small", displayName = "Explicit small output", baseUrl = "http://127.0.0.1:8080/v1",
                        models = new[] { "fixture-model", "fixture-model-two" }, hasApiKey = false, protocol = "openai-completions",
                        contextWindowTokens = 1_000_000, maxOutputTokens = 2048 } } };
                }
                else
                {
                    using var reader = new StreamReader(context.Request.InputStream, Encoding.UTF8);
                    string body = await reader.ReadToEndAsync();
                    using var document = JsonDocument.Parse(body);
                    Check(document.RootElement.TryGetProperty("contextWindowTokens", out var value), "context field missing on outgoing request");
                    Check(value.TryGetInt32(out int tokens), "context field is not an integer");
                    Check(document.RootElement.TryGetProperty("maxOutputTokens", out var output), "output field missing on outgoing request");
                    Check(output.TryGetInt32(out int outputTokens), "output field is not an integer");
                    var connection = JsonSerializer.Deserialize<ModelConnection>(body, json)!;
                    requests.Add((path, tokens, outputTokens));
                    result = path == "/api/models/test"
                        ? new { ok = true, latencyMs = 1, models = new[] { "fixture-model" } }
                        : new { provider = new ModelProvider(connection.ProviderId, connection.DisplayName, connection.BaseUrl,
                            connection.Models, false, connection.Protocol, connection.ContextWindowTokens, connection.MaxOutputTokens) };
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
    Check(providers[0].ContextWindowTokens == 8192 && providers[0].MaxOutputTokens == 262144,
        "legacy response did not use independent context and output defaults");
    Check(providers[1].ContextWindowTokens == 1_000_000 && providers[1].MaxOutputTokens == 2048 && providers[1].Models.Length == 2,
        "existing low output limit or multiple models were changed while loading");
    var connection = new ModelConnection("fixture-local", "Synthetic model", "http://127.0.0.1:8080/v1", ["fixture-model"]);
    var saved = await client.SaveAsync(connection);
    Check(saved.ContextWindowTokens == 8192 && saved.MaxOutputTokens == 262144 && requests[^1].ContextTokens == 8192 && requests[^1].OutputTokens == 262144,
        "new connection defaults were not sent and preserved independently");
    var oneMillion = connection with { ContextWindowTokens = 1_000_000 };
    saved = await client.SaveAsync(oneMillion);
    Check(saved.ContextWindowTokens == 1_000_000 && saved.MaxOutputTokens == 262144 && requests[^1].ContextTokens == 1_000_000,
        "one-million context did not round-trip or changed the output setting");
    var test = await client.TestAsync(oneMillion);
    Check(test.Ok && requests[^1] == ("/api/models/test", 1_000_000, 262144), "connection probe dropped its independent context or output configuration");
    foreach (int tokens in new[] { 2048, 32768, 131072, 262144, 123456, 2000000 })
    {
        saved = await client.SaveAsync(connection with { ContextWindowTokens = tokens });
        Check(saved.ContextWindowTokens == tokens, $"saved configured window {tokens} changed");
    }
    foreach (int tokens in new[] { 1024, 2048, 4096, 8192, 16384, 32768, 65536, 131072, 12345, 262144 })
    {
        var outputConnection = oneMillion with { MaxOutputTokens = tokens };
        saved = await client.SaveAsync(outputConnection);
        Check(saved.MaxOutputTokens == tokens && saved.ContextWindowTokens == 1_000_000 && requests[^1].OutputTokens == tokens,
            $"configured output {tokens} changed or overwrote the one-million context");
        test = await client.TestAsync(outputConnection);
        Check(test.Ok && requests[^1] == ("/api/models/test", 1_000_000, tokens), $"probe dropped configured output {tokens}");
    }
    int before = Volatile.Read(ref requestCount);
    foreach (int invalid in new[] { 0, -1, 2047, 2000001, int.MaxValue })
    {
        await Reject(() => client.SaveAsync(connection with { ContextWindowTokens = invalid }), "invalid context saved");
        await Reject(() => client.TestAsync(connection with { ContextWindowTokens = invalid }), "invalid context tested");
    }
    foreach (int invalid in new[] { 0, -1, 1023, 262145, int.MaxValue })
    {
        await Reject(() => client.SaveAsync(oneMillion with { MaxOutputTokens = invalid }), "invalid output saved");
        await Reject(() => client.TestAsync(oneMillion with { MaxOutputTokens = invalid }), "invalid output tested");
    }
    Check(Volatile.Read(ref requestCount) == before, "invalid windows reached the gateway");
    Check(serverFailure is null, $"fake gateway failed: {serverFailure}");
    Console.WriteLine("PASS: independent legacy 8K context/256K output defaults, explicit 2K output preservation, 1M context, Save/Test output preset/custom/boundary round trips, and invalid limits rejected before network access.");
}
finally
{
    Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", previousAddress);
    listenerShutdown.Cancel();
    listener.Stop();
    await server;
}
