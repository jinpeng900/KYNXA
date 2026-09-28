using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public sealed record ModelProvider(string ProviderId, string DisplayName, string BaseUrl,
    string[] Models, bool HasApiKey, string Protocol);
public sealed record ModelListResponse(ModelProvider[] Providers);
public sealed record ModelProbeResponse(bool Ok, int LatencyMs, string[] Models);
public sealed record ModelSaveResponse(ModelProvider Provider);
public sealed record ModelConnection(string ProviderId, string DisplayName, string BaseUrl,
    string[] Models, string? ApiKey = null, string Protocol = "openai-completions");

public sealed class ModelApiClient : IDisposable
{
    private readonly HttpClient _http = new()
    {
        BaseAddress = new Uri(Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL") ?? "http://127.0.0.1:5218"),
        Timeout = TimeSpan.FromMinutes(5)
    };

    public async Task<ModelProvider[]> ListAsync(CancellationToken cancellationToken = default) =>
        (await ReadAsync<ModelListResponse>(await _http.GetAsync("/api/models", cancellationToken), cancellationToken)).Providers;

    public async Task<ModelProvider> SaveAsync(ModelConnection connection, CancellationToken cancellationToken = default) =>
        (await ReadAsync<ModelSaveResponse>(await _http.PostAsJsonAsync("/api/models", connection, cancellationToken), cancellationToken)).Provider;

    public async Task<ModelProbeResponse> TestAsync(ModelConnection connection, CancellationToken cancellationToken = default) =>
        await ReadAsync<ModelProbeResponse>(await _http.PostAsJsonAsync("/api/models/test", connection, cancellationToken), cancellationToken);

    public async Task<ChatReply> ReplyAsync(ChatRequest request, CancellationToken cancellationToken = default)
    {
        var reply = await ReadAsync<ChatReply>(await _http.PostAsJsonAsync("/api/chat", request, cancellationToken), cancellationToken);
        if (reply.ConversationId != request.ConversationId || reply.Role != "assistant" || string.IsNullOrWhiteSpace(reply.Content))
            throw new InvalidDataException("模型接口返回了无效的回复。");
        return reply;
    }

    private static async Task<T> ReadAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        using (response)
        {
            if (!response.IsSuccessStatusCode)
            {
                try
                {
                    using var body = await response.Content.ReadFromJsonAsync<JsonDocument>(cancellationToken);
                    string? message = body is not null && body.RootElement.TryGetProperty("error", out var field)
                        ? field.GetString() : null;
                    throw new InvalidOperationException(message ?? $"模型接口返回 HTTP {(int)response.StatusCode}。");
                }
                catch (JsonException) { throw new InvalidOperationException($"模型接口返回 HTTP {(int)response.StatusCode}。"); }
            }
            return await response.Content.ReadFromJsonAsync<T>(cancellationToken) ??
                throw new InvalidDataException("模型接口返回了空响应。");
        }
    }

    public void Dispose() => _http.Dispose();
}
