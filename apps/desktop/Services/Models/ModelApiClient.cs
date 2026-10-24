using System.Net.Http.Json;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
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

    private readonly HttpClient _httpClient;
    private readonly Func<CancellationToken, Task> _ensureReady;
    private readonly bool _ownsHttpClient;
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public ModelApiClient() : this(GatewayHttpClient.Create(ModelGatewayService.Address, TimeSpan.FromMinutes(5)),
        ModelGatewayService.EnsureReadyAsync, ownsHttpClient: true) { }

    internal ModelApiClient(HttpClient httpClient, Func<CancellationToken, Task> ensureReady, bool ownsHttpClient = false)
    {
        _httpClient = httpClient;
        _ensureReady = ensureReady;
        _ownsHttpClient = ownsHttpClient;
    }

    public async Task<ModelProvider[]> ListAsync(CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
        using var response = await _httpClient.GetAsync("/api/models", cancellationToken).ConfigureAwait(false);
        return (await ReadAsync<ModelListResponse>(response, cancellationToken).ConfigureAwait(false)).Providers;
    }

    /// <summary>Schedule startup preparation; acceptance does not certify that inference is ready.</summary>
    /// <remarks>仅调度启动准备；接受请求不代表推理已就绪，不读取或传递模型凭据。</remarks>
    public async Task SchedulePreparationAsync(string? providerId, string? modelId, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
        using var response = await _httpClient.PostAsJsonAsync("/api/models/preparation",
            new { providerId, modelId }, cancellationToken).ConfigureAwait(false);
        await ReadAsync<System.Text.Json.JsonElement>(response, cancellationToken).ConfigureAwait(false);
    }

    public async Task<ModelProvider> SaveAsync(ModelConnection connection, CancellationToken cancellationToken = default)
    {
        ValidateContextWindowTokens(connection.ContextWindowTokens);
        ValidateMaxOutputTokens(connection.MaxOutputTokens);
        await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
        using var response = await _httpClient.PostAsJsonAsync("/api/models", connection, cancellationToken).ConfigureAwait(false);
        return (await ReadAsync<ModelSaveResponse>(response, cancellationToken).ConfigureAwait(false)).Provider;
    }

    public async Task<ModelProbeResponse> TestAsync(ModelConnection connection, CancellationToken cancellationToken = default)
    {
        ValidateContextWindowTokens(connection.ContextWindowTokens);
        ValidateMaxOutputTokens(connection.MaxOutputTokens);
        await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
        using var response = await _httpClient.PostAsJsonAsync("/api/models/test", connection, cancellationToken).ConfigureAwait(false);
        return await ReadAsync<ModelProbeResponse>(response, cancellationToken).ConfigureAwait(false);
    }

    public async Task<ChatReply> ReplyAsync(ChatRequest request, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
        using var response = await _httpClient.PostAsJsonAsync("/api/chat", request, cancellationToken).ConfigureAwait(false);
        var reply = await ReadAsync<ChatReply>(response, cancellationToken).ConfigureAwait(false);
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
        await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
        using var response = await _httpClient.PostAsJsonAsync("/api/chat/cancel",
            new { conversationId, requestId }, cancellationToken).ConfigureAwait(false);
        var receipt = await ReadAsync<System.Text.Json.JsonElement>(response, cancellationToken).ConfigureAwait(false);
        if (!receipt.TryGetProperty("cancelled", out var cancelled) || cancelled.ValueKind != System.Text.Json.JsonValueKind.True)
            throw new InvalidDataException(UiText.Get("任务取消结果尚未确认，请查看任务状态。"));
    }

    public async IAsyncEnumerable<ChatStreamEvent> StreamReplyAsync(ChatRequest request,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        request = request with { RequestId = request.RequestId ?? Guid.NewGuid() };
        await foreach (var item in ObserveReplyAsync(request.ConversationId, request.RequestId.Value, null, () =>
        {
            return new HttpRequestMessage(HttpMethod.Post, "/api/chat/stream") { Content = JsonContent.Create(request) };
        }, cancellationToken).ConfigureAwait(false))
            yield return item;
    }

    /// <summary>Observe the resumed task without re-dispatching its original user request.</summary>
    /// <remarks>恢复后仅观察现有任务，避免重发旧请求或重放已完成操作。</remarks>
    public async IAsyncEnumerable<ChatStreamEvent> StreamTaskReplyAsync(Guid conversationId, Guid taskId, Guid requestId,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        if (conversationId == Guid.Empty || taskId == Guid.Empty || requestId == Guid.Empty)
            throw new ArgumentException("A saved chat task is required.");
        await foreach (var item in ObserveReplyAsync(conversationId, requestId, taskId, () => new HttpRequestMessage(HttpMethod.Get,
            $"/api/tasks/{taskId:D}/stream?conversationId={conversationId:D}"), cancellationToken).ConfigureAwait(false))
            yield return item;
    }

    internal async IAsyncEnumerable<ChatStreamEvent> ObserveRequestReplyAsync(Guid conversationId, Guid requestId, Guid? taskId,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        await foreach (var item in ObserveReplyAsync(conversationId, requestId, taskId, () => new HttpRequestMessage(HttpMethod.Get,
            $"/api/chat/stream?conversationId={conversationId:D}&requestId={requestId:D}"), cancellationToken).ConfigureAwait(false))
            yield return item;
    }

    private sealed record ChatRunStatus(Guid ConversationId, Guid RequestId, string Status,
        JsonElement? Result, ChatStreamEvent? ReplyAnchor = null, ToolActivity[]? Tools = null);

    private sealed record StoredReply(Guid Id, string Role, string Content, string Status, string? Reasoning,
        string? Error, DateTimeOffset CreatedAt, long DurationMs, AssistantSegment[]? AssistantSegments,
        ToolActivity[]? ToolActivities, JsonElement? ToolRun, Guid? InReplyToUserMessageId, Guid? ReplyTo);

    private async IAsyncEnumerable<ChatStreamEvent> ObserveReplyAsync(Guid conversationId, Guid requestId,
        Guid? taskId, Func<HttpRequestMessage> initialRequest,
        [EnumeratorCancellation] CancellationToken cancellationToken)
    {
        await _ensureReady(cancellationToken).ConfigureAwait(false);
        bool isInitialRequest = true;
        bool hasStarted = false;
        bool observeDurableTask = false;
        int recoveryAttempt = 0;
        while (true)
        {
            // Submit the prompt once; recovery observes the accepted request without replaying effects.
            // 消息只提交一次；恢复时观察原请求，不重发消息或重做工具操作。
            Func<HttpRequestMessage> nextRequest = isInitialRequest ? initialRequest : () => new HttpRequestMessage(HttpMethod.Get,
                observeDurableTask && taskId is Guid durableTaskId
                    ? $"/api/tasks/{durableTaskId:D}/stream?conversationId={conversationId:D}"
                    : $"/api/chat/stream?conversationId={conversationId:D}&requestId={requestId:D}");
            isInitialRequest = false;
            await using var events = ReadReplyStreamAsync(conversationId, requestId, nextRequest, cancellationToken)
                .GetAsyncEnumerator(cancellationToken);
            Exception? observationError;
            while (true)
            {
                var step = await ReadNextEventAsync(events, cancellationToken).ConfigureAwait(false);
                if (step.Error is { } error) { observationError = error; break; }
                if (step.Event is not { } item)
                {
                    observationError = new EndOfStreamException();
                    break;
                }
                if (item.Type == "started")
                {
                    Guid? observedTask = item.RootRequestTaskId ?? item.TaskId;
                    if (taskId is Guid expected && observedTask is Guid actual && expected != actual)
                    {
                        observationError = new InvalidDataException(UiText.Get("任务接口返回的范围或版本无效。"));
                        break;
                    }
                    taskId ??= observedTask;
                    if (hasStarted) continue;
                    hasStarted = true;
                }
                yield return item;
                if (item.Type is "completed" or "interrupted" or "error") yield break;
            }

            cancellationToken.ThrowIfCancellationRequested();
            if (!CanRecoverObservation(observationError)) throw observationError;
            // Back off malformed snapshot retries while preserving the task and visible content.
            // 异常投影按退避间隔重读，保留任务和已显示内容，避免快速空转。
            int delayMs = recoveryAttempt++ switch { 0 => 100, 1 => 300, 2 => 1000, 3 => 2000, _ => 5000 };
            await Task.Delay(delayMs, cancellationToken).ConfigureAwait(false);
            ChatRunStatus? status = null;
            try
            {
                status = await ReadRunStatusAsync(conversationId, requestId, cancellationToken).ConfigureAwait(false);
                observeDurableTask = false;
            }
            catch (GatewayApiException error) when (error.ErrorCode == "CHAT_RUN_NOT_FOUND")
            {
                observeDurableTask = true;
                if (taskId is null)
                {
                    try { taskId = await FindDurableTaskAsync(conversationId, requestId, cancellationToken).ConfigureAwait(false); }
                    catch (Exception lookupError) when (CanRecoverObservation(lookupError) && !cancellationToken.IsCancellationRequested) { }
                }
            }
            catch (Exception error) when (CanRecoverObservation(error) && !cancellationToken.IsCancellationRequested) { }
            if (observeDurableTask && taskId is Guid durableTask)
            {
                try { status = await ReadDurableRunStatusAsync(conversationId, requestId, durableTask, cancellationToken).ConfigureAwait(false); }
                catch (Exception error) when (CanRecoverObservation(error) && !cancellationToken.IsCancellationRequested) { }
            }
            if (status?.Status != "completed" || status.Result is not { ValueKind: JsonValueKind.Object } result) continue;

            // The terminal projection is authoritative, but it still passes the same identity and segment validator.
            // 正式终态可以补回遗漏回复；仍通过同一身份和段落校验，不能把读取失败当成成功。
            var recovered = await ReadTerminalProjectionAsync(status, result, cancellationToken).ConfigureAwait(false);
            if (recovered is null) continue;
            foreach (var item in recovered)
            {
                if (item.Type == "started")
                {
                    if (hasStarted) continue;
                    hasStarted = true;
                }
                yield return item;
            }
            yield break;
        }
    }

    private async IAsyncEnumerable<ChatStreamEvent> ReadReplyStreamAsync(Guid conversationId, Guid requestId,
        Func<HttpRequestMessage> request, [EnumeratorCancellation] CancellationToken cancellationToken)
    {
        using var message = request();
        message.Headers.Accept.ParseAdd("text/event-stream");
        using var response = await _httpClient.SendAsync(message, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        await GatewayResponseReader.EnsureSuccessAsync(response, UiText.Get("任务接口返回 HTTP {0}。"), cancellationToken).ConfigureAwait(false);
        if (!string.Equals(response.Content.Headers.ContentType?.MediaType, "text/event-stream", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException(UiText.Get("模型接口没有返回流式响应，请重启本地网关后重试。"));
        await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
        await foreach (var item in ChatStreamReader.ReadAsync(stream, conversationId, requestId, cancellationToken).ConfigureAwait(false)) yield return item;
    }

    private static async Task<(ChatStreamEvent? Event, Exception? Error)> ReadNextEventAsync(
        IAsyncEnumerator<ChatStreamEvent> events, CancellationToken cancellationToken)
    {
        try { return await events.MoveNextAsync().ConfigureAwait(false) ? (events.Current, null) : (null, null); }
        catch (Exception error) when (!cancellationToken.IsCancellationRequested && CanRecoverObservation(error))
        { return (null, error); }
    }

    private static bool CanRecoverObservation(Exception error)
    {
        if (error is GatewayApiException { ErrorCode: "MODEL_CREDIT_EXHAUSTED" or "MODEL_AUTHENTICATION_FAILED"
            or "MODEL_ENDPOINT_NOT_FOUND" }) return false;
        return error is InvalidDataException or JsonException or EndOfStreamException or OperationCanceledException
            or KeyNotFoundException or FormatException || error is InvalidOperationException and not GatewayApiException
            || error is GatewayApiException gateway && (gateway.ErrorCode is "CHAT_RUN_NOT_FOUND" or "TASK_STREAM_NOT_READY"
                or "CHAT_REPLAY_CAPACITY_EXCEEDED" || (int)gateway.StatusCode >= 500);
    }

    private async Task<ChatRunStatus> ReadRunStatusAsync(Guid conversationId, Guid requestId, CancellationToken cancellationToken)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(15));
        using var response = await _httpClient.GetAsync(
            $"/api/chat/status?conversationId={conversationId:D}&requestId={requestId:D}", deadline.Token).ConfigureAwait(false);
        var status = await ReadAsync<ChatRunStatus>(response, deadline.Token).ConfigureAwait(false);
        if (status.ConversationId != conversationId || status.RequestId != requestId || status.Status is not ("running" or "completed"))
            throw new InvalidDataException(UiText.Get("模型流返回的会话或请求信息不匹配。"));
        return status;
    }

    private async Task<Guid?> FindDurableTaskAsync(Guid conversationId, Guid requestId, CancellationToken cancellationToken)
    {
        string? cursor = null;
        for (int pageIndex = 0; pageIndex < 3; pageIndex++)
        {
            string path = $"/api/tasks?conversationId={conversationId:D}";
            if (cursor is not null) path += "&afterTaskId=" + Uri.EscapeDataString(cursor);
            using var response = await _httpClient.GetAsync(path, cancellationToken).ConfigureAwait(false);
            var page = await ReadAsync<JsonElement>(response, cancellationToken).ConfigureAwait(false);
            foreach (var task in page.GetProperty("tasks").EnumerateArray())
            {
                if (!Guid.TryParse(task.GetProperty("requestId").GetString(), out var originalRequest) || originalRequest != requestId) continue;
                if (task.GetProperty("scope").GetProperty("conversationId").GetGuid() != conversationId
                    || task.TryGetProperty("parentTaskId", out var parent) && parent.ValueKind != JsonValueKind.Null)
                    throw new InvalidDataException(UiText.Get("任务接口返回的范围或版本无效。"));
                return task.GetProperty("taskId").GetGuid();
            }
            if (!page.TryGetProperty("hasMore", out var hasMore) || hasMore.ValueKind != JsonValueKind.True
                || !page.TryGetProperty("nextTaskId", out var next) || next.ValueKind != JsonValueKind.String) break;
            cursor = next.GetString();
        }
        return null;
    }

    private async Task<ChatRunStatus> ReadDurableRunStatusAsync(Guid conversationId, Guid requestId, Guid taskId,
        CancellationToken cancellationToken)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(30));
        using var response = await _httpClient.GetAsync(
            $"/api/tasks/{taskId:D}?conversationId={conversationId:D}", deadline.Token).ConfigureAwait(false);
        var detail = await ReadAsync<JsonElement>(response, deadline.Token).ConfigureAwait(false);
        var task = detail.GetProperty("task");
        if (task.GetProperty("taskId").GetGuid() != taskId || task.GetProperty("scope").GetProperty("conversationId").GetGuid() != conversationId
            || !Guid.TryParse(task.GetProperty("requestId").GetString(), out var originalRequest) || originalRequest != requestId
            || task.TryGetProperty("parentTaskId", out var parent) && parent.ValueKind != JsonValueKind.Null)
            throw new InvalidDataException(UiText.Get("任务接口返回的范围或版本无效。"));
        string? taskState = task.GetProperty("state").GetString();
        bool waitingForUser = taskState is "paused" or "waiting-config" or "waiting-verification";
        if (!waitingForUser && taskState is not ("completed" or "partial" or "failed" or "cancelled"))
            return new(conversationId, requestId, "running", null);

        // After restart, restore terminal state only from the matching durable task and message.
        // 重启后仅用同一持久任务和正式消息补齐终态，不重做原请求。
        Guid replyId = requestId;
        if (detail.TryGetProperty("checkpoint", out var checkpoint) && checkpoint.ValueKind == JsonValueKind.Object
            && checkpoint.TryGetProperty("contextReference", out var context) && context.ValueKind == JsonValueKind.Object
            && context.TryGetProperty("assistantMessageId", out var assistantId) && assistantId.TryGetGuid(out var savedId)) replyId = savedId;
        string? cursor = null;
        for (int pageIndex = 0; pageIndex < 3; pageIndex++)
        {
            string path = $"/api/conversations/{conversationId:D}/messages?limit=200&maxBytes=8388608";
            if (cursor is not null) path += "&cursor=" + Uri.EscapeDataString(cursor);
            using var historyResponse = await _httpClient.GetAsync(path, deadline.Token).ConfigureAwait(false);
            var page = await ReadAsync<JsonElement>(historyResponse, deadline.Token).ConfigureAwait(false);
            if (page.GetProperty("conversationId").GetGuid() != conversationId)
                throw new InvalidDataException(UiText.Get("模型流返回的会话或请求信息不匹配。"));
            var stored = page.GetProperty("messages").EnumerateArray().FirstOrDefault(message =>
                (message.TryGetProperty("Id", out var id) || message.TryGetProperty("id", out id))
                && id.TryGetGuid(out var candidate) && candidate == replyId);
            if (stored.ValueKind == JsonValueKind.Object)
            {
                var reply = stored.Deserialize<StoredReply>(JsonOptions)
                    ?? throw new InvalidDataException(UiText.Get("模型接口返回了无效的回复。"));
                if (reply.Role != "assistant" || !waitingForUser && reply.Status is not ("completed" or "interrupted" or "error")) break;
                JsonElement? completion = reply.ToolRun is { ValueKind: JsonValueKind.Object } toolRun
                    && toolRun.TryGetProperty("taskCompletion", out var outcome) ? outcome.Clone() : null;
                var segments = waitingForUser ? reply.AssistantSegments?.Select(segment => segment.Status == "streaming"
                    ? segment with { Status = "interrupted" } : segment).ToArray() : reply.AssistantSegments;
                var terminal = new ChatStreamEvent(waitingForUser ? "interrupted" : reply.Status, conversationId, requestId, reply.CreatedAt,
                    Content: reply.Content, Reasoning: reply.Reasoning, Error: reply.Error,
                    AssistantSegments: segments is { Length: > 0 } ? segments : null,
                    ToolStreamProtocol: segments is { Length: > 0 } ? 3 : null,
                    DurationMs: reply.DurationMs, TaskCompletion: completion, TaskId: taskId,
                    RootRequestTaskId: taskId, ReplyMessageId: reply.Id, Code: taskState == "paused" ? "TASK_PAUSED" : null);
                ChatStreamEvent? anchor = reply.Id != requestId && (reply.ReplyTo ?? reply.InReplyToUserMessageId) is Guid userId
                    ? new("reply_anchor", conversationId, requestId, reply.CreatedAt, TaskId: taskId,
                        RootRequestTaskId: taskId, ReplyMessageId: reply.Id, UserMessageId: userId) : null;
                return new(conversationId, requestId, "completed", JsonSerializer.SerializeToElement(terminal, JsonOptions),
                    anchor, reply.ToolActivities);
            }
            if (!page.TryGetProperty("hasMore", out var hasMore) || hasMore.ValueKind != JsonValueKind.True
                || !page.TryGetProperty("nextCursor", out var nextCursor) || nextCursor.ValueKind != JsonValueKind.String) break;
            cursor = nextCursor.GetString();
        }
        return new(conversationId, requestId, "running", null);
    }

    private static async Task<List<ChatStreamEvent>?> ReadTerminalProjectionAsync(ChatRunStatus status, JsonElement result,
        CancellationToken cancellationToken)
    {
        var started = new ChatStreamEvent("started", status.ConversationId, status.RequestId, DateTimeOffset.UtcNow);
        string wire = "data: " + JsonSerializer.Serialize(started, JsonOptions) + "\n\n";
        if (status.ReplyAnchor is { } anchor) wire += "data: " + JsonSerializer.Serialize(anchor, JsonOptions) + "\n\n";
        foreach (var tool in status.Tools ?? [])
        {
            if (tool.Status is not ("completed" or "error" or "cancelled" or "unknown") || tool.Result is null) continue;
            var call = new ChatStreamEvent("tool_call", status.ConversationId, status.RequestId, started.CreatedAt,
                Tool: tool with { Status = "running", ApprovalId = null });
            var receipt = call with { Type = "tool_result", Tool = tool with { ApprovalId = null } };
            wire += "data: " + JsonSerializer.Serialize(call, JsonOptions) + "\n\n";
            wire += "data: " + JsonSerializer.Serialize(receipt, JsonOptions) + "\n\n";
        }
        wire += "data: " + result.GetRawText() + "\n\n";
        using var stream = new MemoryStream(Encoding.UTF8.GetBytes(wire));
        var events = new List<ChatStreamEvent>();
        try
        {
            await foreach (var item in ChatStreamReader.ReadAsync(stream, status.ConversationId, status.RequestId, cancellationToken)
                .ConfigureAwait(false)) events.Add(item);
        }
        catch (Exception error) when (error is InvalidDataException or JsonException) { return null; }
        return events.Count > 0 && events[^1].Type is "completed" or "interrupted" or "error" ? events : null;
    }

    private static Task<T> ReadAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken) =>
        GatewayResponseReader.ReadAsync<T>(response, UiText.Get("模型接口返回了空响应。"),
            UiText.Get("模型接口返回 HTTP {0}。"), cancellationToken);

    public void Dispose() { if (_ownsHttpClient) _httpClient.Dispose(); }
}
