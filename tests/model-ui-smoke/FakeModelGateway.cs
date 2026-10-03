using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Services;

namespace ModelUiSmoke;

internal sealed record ReceivedModelRequest(string Path, ModelConnection Connection);

/// <summary>A loopback-only gateway that records synthetic model settings and never calls an upstream model.</summary>
internal sealed class FakeModelGateway : IAsyncDisposable
{
    private readonly HttpListener _listener = new();
    private readonly CancellationTokenSource _shutdown = new();
    private readonly JsonSerializerOptions _json = new(JsonSerializerDefaults.Web);
    private readonly Dictionary<string, ModelProvider> _providers = new(StringComparer.Ordinal);
    private readonly List<Task> _responses = [];
    private readonly object _sync = new();
    private readonly Task _listening;
    private bool _legacyMissingLimits = true;
    private int _listReads, _saves, _probes;

    public string Address { get; }
    public int ListReads => Volatile.Read(ref _listReads);
    public int Saves => Volatile.Read(ref _saves);
    public int Probes => Volatile.Read(ref _probes);
    public int ProbeDelayMs { get; set; }
    public Exception? Failure { get; private set; }
    public ConcurrentQueue<ReceivedModelRequest> Requests { get; } = new();

    public FakeModelGateway()
    {
        using var reservation = new TcpListener(IPAddress.Loopback, 0);
        reservation.Start();
        int port = ((IPEndPoint)reservation.LocalEndpoint).Port;
        reservation.Stop();
        Address = $"http://127.0.0.1:{port}";
        _listener.Prefixes.Add(Address + "/");
        _listener.Start();
        _providers.Add("fixture-legacy", new("fixture-legacy", "Legacy / 旧连接", "http://127.0.0.1:8080/v1",
            ["fixture-model"], false, "openai-completions"));
        _providers.Add("fixture-small", new("fixture-small", "Small / 两个模型", "https://models.test.invalid/v1",
            ["fixture-model", "fixture-model-two"], true, "openai-responses", 1_000_000, 2048));
        _listening = ListenAsync();
    }

    private async Task ListenAsync()
    {
        try
        {
            while (!_shutdown.IsCancellationRequested)
            {
                var context = await _listener.GetContextAsync();
                var response = RespondAsync(context);
                lock (_sync) _responses.Add(response);
            }
        }
        catch (Exception error) when (_shutdown.IsCancellationRequested &&
            error is HttpListenerException or ObjectDisposedException or InvalidOperationException) { }
        catch (Exception error) { Failure = error; }
    }

    private object[] ProviderResponse()
    {
        lock (_sync)
        {
            return _providers.Values.Select(provider => provider.ProviderId == "fixture-legacy" && _legacyMissingLimits
                ? (object)new { provider.ProviderId, provider.DisplayName, provider.BaseUrl, provider.Models,
                    provider.HasApiKey, provider.Protocol }
                : provider).ToArray();
        }
    }

    private async Task RespondAsync(HttpListenerContext context)
    {
        try
        {
            object result;
            string path = context.Request.Url!.AbsolutePath;
            if (path == "/health")
            {
                result = new { service = "kynxa-model-gateway", status = "ok", storageProtocol = 1,
                    conversationProtocol = 1, memoryProtocol = 1, contextProtocol = 3, agentProtocol = 5,
                    extensionStorageProtocol = 1, toolStreamProtocol = 3, dataLayoutVersion = 1 };
            }
            else if (path == "/api/models" && context.Request.HttpMethod == "GET")
            {
                Interlocked.Increment(ref _listReads);
                result = new { providers = ProviderResponse() };
            }
            else if (context.Request.HttpMethod == "POST" && path is "/api/models" or "/api/models/test")
            {
                using var reader = new StreamReader(context.Request.InputStream, Encoding.UTF8);
                string body = await reader.ReadToEndAsync(_shutdown.Token);
                using var document = JsonDocument.Parse(body);
                if (!document.RootElement.TryGetProperty("maxOutputTokens", out var output) || !output.TryGetInt32(out _) ||
                    !document.RootElement.TryGetProperty("contextWindowTokens", out var window) || !window.TryGetInt32(out _))
                    throw new InvalidDataException("The native form did not send both integer token limits.");
                var connection = JsonSerializer.Deserialize<ModelConnection>(body, _json)!;
                Requests.Enqueue(new(path, connection));
                if (path == "/api/models/test")
                {
                    Interlocked.Increment(ref _probes);
                    if (ProbeDelayMs > 0) await Task.Delay(ProbeDelayMs, _shutdown.Token);
                    result = new { ok = true, latencyMs = 2, models = new[] { "fixture-model", "fixture-model-two" } };
                }
                else
                {
                    ModelProvider provider;
                    lock (_sync)
                    {
                        bool hasApiKey = !string.IsNullOrWhiteSpace(connection.ApiKey) ||
                            _providers.TryGetValue(connection.ProviderId, out var previous) && previous.HasApiKey;
                        provider = new(connection.ProviderId, connection.DisplayName, connection.BaseUrl, connection.Models,
                            hasApiKey, connection.Protocol, connection.ContextWindowTokens, connection.MaxOutputTokens);
                        _providers[provider.ProviderId] = provider;
                        if (provider.ProviderId == "fixture-legacy") _legacyMissingLimits = false;
                    }
                    Interlocked.Increment(ref _saves);
                    result = new { provider };
                }
            }
            else
            {
                context.Response.StatusCode = 404;
                result = new { error = "FIXTURE_ROUTE_NOT_FOUND" };
            }
            byte[] bytes = JsonSerializer.SerializeToUtf8Bytes(result, _json);
            context.Response.ContentType = "application/json";
            context.Response.ContentLength64 = bytes.Length;
            await context.Response.OutputStream.WriteAsync(bytes, _shutdown.Token);
        }
        catch (Exception error) when (_shutdown.IsCancellationRequested &&
            error is OperationCanceledException or ObjectDisposedException or HttpListenerException or IOException) { }
        catch (Exception error) when (error is HttpListenerException or IOException)
        {
            // Closing the owned native window aborts its pending HTTP response.
        }
        catch (Exception error) { Failure = error; }
        finally { context.Response.Close(); }
    }

    public async ValueTask DisposeAsync()
    {
        _shutdown.Cancel();
        _listener.Stop();
        await _listening;
        Task[] responses;
        lock (_sync) responses = _responses.ToArray();
        await Task.WhenAll(responses);
        _listener.Close();
        _shutdown.Dispose();
    }
}
