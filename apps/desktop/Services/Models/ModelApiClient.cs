using System.Net.Http.Json;
using System.Runtime.CompilerServices;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public sealed record ModelProvider(string ProviderId, string DisplayName, string BaseUrl,
    string[] Models, bool HasApiKey, string Protocol, int ContextWindowTokens = ModelApiClient.DefaultContextWindowTokens,
    int MaxOutputTokens = ModelApiClient.DefaultMaxOutputTokens);
public sealed record ModelListResponse(ModelProvider[] Providers);
public sealed record ModelProbeResponse(bool Ok, int LatencyMs, string[] Models);
public sealed record ModelSaveResponse(ModelProvider Provider);
public sealed record ModelConnection(string ProviderId, string DisplayName, string BaseUrl,
    string[] Models, string? ApiKey = null, string Protocol = "openai-completions",
    int ContextWindowTokens = ModelApiClient.DefaultContextWindowTokens,
    int MaxOutputTokens = ModelApiClient.DefaultMaxOutputTokens);

public sealed class ModelApiClient : IDisposable
{
    public const int DefaultContextWindowTokens = 32768;
    public const int MinimumContextWindowTokens = 2048;
    public const int MaximumContextWindowTokens = 2000000;
    public const int DefaultMaxOutputTokens = 262144;
    public const int MinimumMaxOutputTokens = 1024;
    public const int MaximumMaxOutputTokens = 262144;

    public static int ValidateContextWindowTokens(int value)
    {
        if (value is < MinimumContextWindowTokens or > MaximumContextWindowTokens)
            throw new InvalidOperationException(UiText.Get("上下文窗口须为 2048–2000000 的整数（tokens）。"));
        return value;
    }

    public static int ValidateMaxOutputTokens(int value)
    {
        if (value is < MinimumMaxOutputTokens or > MaximumMaxOutputTokens)
            throw new InvalidOperationException(UiText.Get("最大输出须为 1024–262144 的整数（tokens）。"));
        return value;
    }

    private readonly HttpClient _httpClient = new()
    {
        BaseAddress = ModelGatewayService.Address,
        Timeout = TimeSpan.FromMinutes(5)
    };

    public async Task<ModelProvider[]> ListAsync(CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var response = await _httpClient.GetAsync("/api/models", cancellationToken);
        return (await ReadAsync<ModelListResponse>(response, cancellationToken)).Providers;
    }

    /// <summary>Schedule startup preparation; acceptance does not certify that inference is ready.</summary>
    /// <remarks>仅调度启动准备；接受请求不代表推理已就绪，不读取或传递模型凭据。</remarks>
    public async Task SchedulePreparationAsync(string? providerId, string? modelId, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var response = await _httpClient.PostAsJsonAsync("/api/models/preparation",
            new { providerId, modelId }, cancellationToken);
        await ReadAsync<System.Text.Json.JsonElement>(response, cancellationToken);
    }

    public async Task<ModelProvider> SaveAsync(ModelConnection connection, CancellationToken cancellationToken = default)
    {
        ValidateContextWindowTokens(connection.ContextWindowTokens);
        ValidateMaxOutputTokens(connection.MaxOutputTokens);
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var response = await _httpClient.PostAsJsonAsync("/api/models", connection, cancellationToken);
        return (await ReadAsync<ModelSaveResponse>(response, cancellationToken)).Provider;
    }

    public async Task<ModelProbeResponse> TestAsync(ModelConnection connection, CancellationToken cancellationToken = default)
    {
        ValidateContextWindowTokens(connection.ContextWindowTokens);
        ValidateMaxOutputTokens(connection.MaxOutputTokens);
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var response = await _httpClient.PostAsJsonAsync("/api/models/test", connection, cancellationToken);
        return await ReadAsync<ModelProbeResponse>(response, cancellationToken);
    }

    public async Task<ChatReply> ReplyAsync(ChatRequest request, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var response = await _httpClient.PostAsJsonAsync("/api/chat", request, cancellationToken);
        var reply = await ReadAsync<ChatReply>(response, cancellationToken);
        if (reply.ConversationId != request.ConversationId || reply.Role != "assistant" || string.IsNullOrWhiteSpace(reply.Content) ||
            !ChatDurationRules.IsValid(reply.DurationMs))
            throw new InvalidDataException(UiText.Get("模型接口返回了无效的回复。"));
        return reply;
    }

    /// <summary>Explicit user cancellation is separate from disconnecting a reply stream.</summary>
    /// <remarks>用户明确取消与关闭回复观察连接分开；不把 HTTP 失败当作取消成功。</remarks>
    public async Task CancelReplyAsync(Guid conversationId, Guid requestId, CancellationToken cancellationToken = default)
    {
        if (conversationId == Guid.Empty || requestId == Guid.Empty) throw new ArgumentException("A saved chat request is required.");
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var response = await _httpClient.PostAsJsonAsync("/api/chat/cancel",
            new { conversationId, requestId }, cancellationToken);
        var receipt = await ReadAsync<System.Text.Json.JsonElement>(response, cancellationToken);
        if (!receipt.TryGetProperty("cancelled", out var cancelled) || cancelled.ValueKind != System.Text.Json.JsonValueKind.True)
            throw new InvalidDataException(UiText.Get("任务取消结果尚未确认，请查看任务状态。"));
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
        using var response = await _httpClient.SendAsync(message, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        await GatewayResponseReader.EnsureSuccessAsync(response, UiText.Get("模型接口返回 HTTP {0}。"), cancellationToken);
        if (!string.Equals(response.Content.Headers.ContentType?.MediaType, "text/event-stream", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException(UiText.Get("模型接口没有返回流式响应，请重启本地网关后重试。"));
        await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken);
        await foreach (var item in ChatStreamReader.ReadAsync(stream, request.ConversationId, request.RequestId.Value, cancellationToken))
            yield return item;
    }

    /// <summary>Observe the resumed task without re-dispatching its original user request.</summary>
    /// <remarks>恢复后仅观察现有任务，避免重发旧请求或重放已完成操作。</remarks>
    public async IAsyncEnumerable<ChatStreamEvent> StreamTaskReplyAsync(Guid conversationId, Guid taskId, Guid requestId,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        if (conversationId == Guid.Empty || taskId == Guid.Empty || requestId == Guid.Empty)
            throw new ArgumentException("A saved chat task is required.");
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var message = new HttpRequestMessage(HttpMethod.Get,
            $"/api/tasks/{taskId:D}/stream?conversationId={conversationId:D}");
        message.Headers.Accept.ParseAdd("text/event-stream");
        using var response = await _httpClient.SendAsync(message, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        await GatewayResponseReader.EnsureSuccessAsync(response, UiText.Get("任务接口返回 HTTP {0}。"), cancellationToken);
        if (!string.Equals(response.Content.Headers.ContentType?.MediaType, "text/event-stream", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException(UiText.Get("模型接口没有返回流式响应，请重启本地网关后重试。"));
        await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken);
        await foreach (var item in ChatStreamReader.ReadAsync(stream, conversationId, requestId, cancellationToken)) yield return item;
    }

    private static Task<T> ReadAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken) =>
        GatewayResponseReader.ReadAsync<T>(response, UiText.Get("模型接口返回了空响应。"),
            UiText.Get("模型接口返回 HTTP {0}。"), cancellationToken);

    public void Dispose() => _httpClient.Dispose();
}
