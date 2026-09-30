using System.Net.Http.Json;
using System.Runtime.CompilerServices;
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
        BaseAddress = ModelGatewayService.Address,
        Timeout = TimeSpan.FromMinutes(5)
    };

    public async Task<ModelProvider[]> ListAsync(CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        return (await ReadAsync<ModelListResponse>(await _http.GetAsync("/api/models", cancellationToken), cancellationToken)).Providers;
    }

    public async Task<ModelProvider> SaveAsync(ModelConnection connection, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        return (await ReadAsync<ModelSaveResponse>(await _http.PostAsJsonAsync("/api/models", connection, cancellationToken), cancellationToken)).Provider;
    }

    public async Task<ModelProbeResponse> TestAsync(ModelConnection connection, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        return await ReadAsync<ModelProbeResponse>(await _http.PostAsJsonAsync("/api/models/test", connection, cancellationToken), cancellationToken);
    }

    public async Task<ChatReply> ReplyAsync(ChatRequest request, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        var reply = await ReadAsync<ChatReply>(await _http.PostAsJsonAsync("/api/chat", request, cancellationToken), cancellationToken);
        if (reply.ConversationId != request.ConversationId || reply.Role != "assistant" || string.IsNullOrWhiteSpace(reply.Content))
            throw new InvalidDataException("模型接口返回了无效的回复。");
        return reply;
    }

    public async IAsyncEnumerable<ChatStreamEvent> StreamReplyAsync(ChatRequest request,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        request = request with { RequestId = request.RequestId ?? Guid.NewGuid() };
        using var message = new HttpRequestMessage(HttpMethod.Post, "/api/chat/stream")
        {
            Content = JsonContent.Create(request)
        };
        message.Headers.Accept.ParseAdd("text/event-stream");
        using var response = await _http.SendAsync(message, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        await EnsureSuccessAsync(response, cancellationToken);
        if (!string.Equals(response.Content.Headers.ContentType?.MediaType, "text/event-stream", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("模型接口没有返回流式响应，请重启本地网关后重试。");
        await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken);
        await foreach (var item in ChatStreamReader.ReadAsync(stream, request.ConversationId, request.RequestId.Value, cancellationToken))
            yield return item;
    }

    private static async Task<T> ReadAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        using (response)
        {
            await EnsureSuccessAsync(response, cancellationToken);
            return await response.Content.ReadFromJsonAsync<T>(cancellationToken) ??
                throw new InvalidDataException("模型接口返回了空响应。");
        }
    }

    private static async Task EnsureSuccessAsync(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        if (response.IsSuccessStatusCode) return;
        try
        {
            using var body = await response.Content.ReadFromJsonAsync<JsonDocument>(cancellationToken);
            string? message = body is not null && body.RootElement.TryGetProperty("error", out var field) &&
                field.ValueKind == JsonValueKind.String ? field.GetString() : null;
            throw new InvalidOperationException(message ?? $"模型接口返回 HTTP {(int)response.StatusCode}。");
        }
        catch (JsonException) { throw new InvalidOperationException($"模型接口返回 HTTP {(int)response.StatusCode}。"); }
    }

    public void Dispose() => _http.Dispose();
}
