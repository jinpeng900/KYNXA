using System.Net.Http.Json;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public interface ITasksApi
{
    Task<TaskListResponse> ListAsync(Guid conversationId, Guid? projectId = null, Guid? afterTaskId = null,
        CancellationToken cancellationToken = default);
    Task<TaskPausedResponse> PausedAsync(Guid conversationId, Guid? projectId = null,
        CancellationToken cancellationToken = default);
    Task<TaskListResponse> RecoverableAsync(Guid conversationId, Guid? projectId = null, Guid? afterTaskId = null,
        CancellationToken cancellationToken = default);
    Task<TaskDetailResponse> GetAsync(Guid conversationId, Guid taskId, Guid? projectId = null,
        CancellationToken cancellationToken = default);
    Task<TaskEventsResponse> EventsAsync(Guid conversationId, Guid taskId, ulong afterSequence,
        CancellationToken cancellationToken = default);
    Task<TaskControlResponse> ControlAsync(Guid conversationId, Guid taskId, ulong expectedVersion, string action,
        Guid? projectId = null, CancellationToken cancellationToken = default);
    Task<TaskControlResponse> SteerAsync(Guid conversationId, Guid taskId, ulong expectedVersion,
        Guid userMessageId, string message, string action, Guid? projectId = null,
        CancellationToken cancellationToken = default, Guid? replyMessageId = null);
}

/// <summary>
/// Reads durable task projections and submits explicit versioned controls; owns no task state.
/// 读取持久任务投影并提交明确的带版本控制；不拥有正式任务状态。
/// </summary>
public sealed class TasksApiClient : ITasksApi, IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http;
    private readonly Func<CancellationToken, Task> _ensureReady;
    private readonly bool _ownsHttp;

    public TasksApiClient() : this(GatewayHttpClient.Create(ModelGatewayService.Address, Timeout.InfiniteTimeSpan),
        ModelGatewayService.EnsureReadyAsync, ownsHttp: true) { }

    public TasksApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttp = false)
    {
        _http = http; _ensureReady = ensureReady ?? (_ => Task.CompletedTask); _ownsHttp = ownsHttp;
    }

    public Task<TaskListResponse> ListAsync(Guid conversationId, Guid? projectId = null,
        Guid? afterTaskId = null, CancellationToken cancellationToken = default) =>
        ReadListAsync(conversationId, projectId, afterTaskId, true, cancellationToken);

    // Reopening one chat must not page through other chats in the same project.
    // 重开单个聊天仅查询该聊天；仍用预期项目复核归属，不分页读取同项目其他聊天。
    internal Task<TaskListResponse> ListConversationAsync(Guid conversationId, Guid? projectId = null,
        Guid? afterTaskId = null, CancellationToken cancellationToken = default) =>
        ReadListAsync(conversationId, projectId, afterTaskId, false, cancellationToken);

    private async Task<TaskListResponse> ReadListAsync(Guid conversationId, Guid? projectId,
        Guid? afterTaskId, bool includeProjectConversations, CancellationToken cancellationToken)
    {
        using var deadline = CreateDeadline(cancellationToken, 30);
        cancellationToken = deadline.Token;
        string path = $"/api/tasks?conversationId={ValidId(conversationId):D}";
        if (includeProjectConversations && projectId is Guid project) path += $"&projectId={ValidId(project):D}";
        if (afterTaskId is Guid after) path += $"&afterTaskId={ValidId(after):D}";
        await _ensureReady(cancellationToken);
        var result = await ReadProjectionAsync<TaskListResponse>(path, cancellationToken);
        if (result.Tasks is null || result.Tasks.Length > 100 || (result.HasMore && result.NextTaskId is not { } next)
            || (result.NextTaskId is Guid nextId && (nextId == Guid.Empty || nextId == afterTaskId))) throw Invalid();
        foreach (var task in result.Tasks) Validate(task, conversationId, projectId,
            allowProjectConversations: includeProjectConversations && projectId is not null);
        if (result.Tasks.Select(task => task.TaskId).Distinct().Count() != result.Tasks.Length) throw Invalid();
        return result;
    }

    public async Task<TaskPausedResponse> PausedAsync(Guid conversationId, Guid? projectId = null,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CreateDeadline(cancellationToken, 30);
        cancellationToken = deadline.Token;
        string path = $"/api/tasks/paused?conversationId={ValidId(conversationId):D}";
        if (projectId is Guid project) path += $"&projectId={ValidId(project):D}";
        await _ensureReady(cancellationToken);
        var result = await ReadProjectionAsync<TaskPausedResponse>(path, cancellationToken);
        if (result.Tasks is null || result.Tasks.Length > 2) throw Invalid();
        foreach (var task in result.Tasks)
        {
            Validate(task, conversationId, projectId);
            if (task.ParentTaskId is not null || task.State != "paused") throw Invalid();
        }
        if (result.Tasks.Select(task => task.TaskId).Distinct().Count() != result.Tasks.Length) throw Invalid();
        return result;
    }

    public async Task<TaskListResponse> RecoverableAsync(Guid conversationId, Guid? projectId = null,
        Guid? afterTaskId = null, CancellationToken cancellationToken = default)
    {
        using var deadline = CreateDeadline(cancellationToken, 30);
        cancellationToken = deadline.Token;
        string path = $"/api/tasks/recoverable?conversationId={ValidId(conversationId):D}&limit=50";
        if (projectId is Guid project) path += $"&projectId={ValidId(project):D}";
        if (afterTaskId is Guid after) path += $"&afterTaskId={ValidId(after):D}";
        await _ensureReady(cancellationToken);
        var result = await ReadProjectionAsync<TaskListResponse>(path, cancellationToken);
        if (result.Tasks is null || result.Tasks.Length > 50 || result.HasMore && result.NextTaskId is null ||
            result.NextTaskId is Guid next && (next == Guid.Empty || next == afterTaskId)) throw Invalid();
        foreach (var task in result.Tasks)
        {
            Validate(task, conversationId, projectId);
            if (task.ParentTaskId is not null || !TaskStates.CanResume(task.State)) throw Invalid();
        }
        if (result.Tasks.Select(task => task.TaskId).Distinct().Count() != result.Tasks.Length) throw Invalid();
        return result;
    }

    public async Task<TaskDetailResponse> GetAsync(Guid conversationId, Guid taskId, Guid? projectId = null,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CreateDeadline(cancellationToken, 30);
        cancellationToken = deadline.Token;
        await _ensureReady(cancellationToken);
        var result = await ReadProjectionAsync<TaskDetailResponse>(PathFor(conversationId, taskId), cancellationToken);
        Validate(result.Task, conversationId, projectId, taskId);
        if (result.Children is null || result.Operations is null || result.Artifacts is null) throw Invalid();
        foreach (var child in result.Children)
        {
            Validate(child, conversationId, projectId);
            if (child.ParentTaskId != taskId) throw Invalid();
        }
        if (result.Operations.Any(operation => operation.TaskId != taskId)) throw Invalid();
        return result;
    }

    public async Task<TaskEventsResponse> EventsAsync(Guid conversationId, Guid taskId, ulong afterSequence,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CreateDeadline(cancellationToken, 30);
        cancellationToken = deadline.Token;
        string path = $"/api/tasks/{ValidId(taskId):D}/events?conversationId={ValidId(conversationId):D}&afterSequence={afterSequence}";
        await _ensureReady(cancellationToken);
        var result = await ReadProjectionAsync<TaskEventsResponse>(path, cancellationToken);
        if (result.TaskId != taskId || result.Events is null || result.Events.Length > 1000 || result.ThroughSequence < afterSequence)
            throw Invalid();
        ulong previous = afterSequence;
        foreach (var item in result.Events)
        {
            if (item.TaskId != taskId || item.Sequence <= previous || item.Sequence > result.ThroughSequence) throw Invalid();
            previous = item.Sequence;
        }
        return result;
    }

    public async Task<TaskControlResponse> ControlAsync(Guid conversationId, Guid taskId, ulong expectedVersion,
        string action, Guid? projectId = null, CancellationToken cancellationToken = default)
    {
        if (action is not ("pause" or "resume" or "cancel")) throw new ArgumentException("Invalid task control.");
        using var deadline = CreateDeadline(cancellationToken, 30);
        cancellationToken = deadline.Token;
        if (expectedVersion == 0) throw new ArgumentException("Invalid task control.");
        string path = $"/api/tasks/{ValidId(taskId):D}/control";
        await _ensureReady(cancellationToken);
        using var response = await _http.PostAsJsonAsync(path,
            new TaskControlRequest(ValidId(conversationId), expectedVersion, action), JsonOptions, cancellationToken);
        var result = await ReadAsync<TaskControlResponse>(response, cancellationToken);
        Validate(result.Task, conversationId, projectId, taskId);
        if (result.AffectedTaskIds is null || result.AffectedTaskIds.Any(id => id == Guid.Empty)) throw Invalid();
        return result;
    }

    public Task<TaskControlResponse> SteerAsync(Guid conversationId, Guid taskId, ulong expectedVersion,
        Guid userMessageId, string message, string action, Guid? projectId = null,
        CancellationToken cancellationToken = default, Guid? replyMessageId = null) =>
        SteerAsync(conversationId, taskId, expectedVersion, userMessageId, message, action, projectId,
            cancellationToken, replyMessageId, null);

    public async Task<TaskControlResponse> SteerAsync(Guid conversationId, Guid taskId, ulong expectedVersion,
        Guid userMessageId, string message, string action, Guid? projectId,
        CancellationToken cancellationToken, Guid? replyMessageId, ChatAttachment[]? attachments)
    {
        // Running-task steering keeps the live attempt; resuming a paused task may await lease release.
        // 运行中的引导保留当前尝试；恢复暂停任务时可能等待旧租约释放。
        using var deadline = CreateDeadline(cancellationToken, 60);
        cancellationToken = deadline.Token;
        if (action is not ("continue" or "update") || expectedVersion == 0 || (string.IsNullOrWhiteSpace(message) && !(attachments?.Length > 0))
            || Encoding.UTF8.GetByteCount(message) > 32768) throw new ArgumentException("Invalid task steering.");
        string path = $"/api/tasks/{ValidId(taskId):D}/steer";
        await _ensureReady(cancellationToken);
        using var response = await _http.PostAsJsonAsync(path,
            new TaskSteeringRequest(ValidId(conversationId), expectedVersion, ValidId(userMessageId), message, action, replyMessageId, attachments),
            JsonOptions, cancellationToken);
        var result = await ReadAsync<TaskControlResponse>(response, cancellationToken);
        Validate(result.Task, conversationId, projectId, taskId);
        if (result.AffectedTaskIds is null || result.AffectedTaskIds.Any(id => id == Guid.Empty)) throw Invalid();
        return result;
    }

    private static string PathFor(Guid conversationId, Guid taskId) =>
        $"/api/tasks/{ValidId(taskId):D}?conversationId={ValidId(conversationId):D}";
    private static Guid ValidId(Guid id) => id != Guid.Empty ? id : throw new ArgumentException("A saved task/chat ID is required.");
    private static void Validate(TaskRecord? task, Guid conversationId, Guid? projectId, Guid? taskId = null,
        bool allowProjectConversations = false)
    {
        if (task is null || task.TaskId == Guid.Empty || (taskId is Guid expected && task.TaskId != expected)
            || string.IsNullOrWhiteSpace(task.RequestId) || task.RequestId.Length > 240
            || task.Scope is null || task.Scope.ConversationId == Guid.Empty || task.Scope.ProjectId != projectId
            || (task.Scope.ConversationId != conversationId && !(allowProjectConversations && projectId is not null))
            || !TaskStates.IsSupported(task.State) || task.Version == 0 || task.Usage is null || task.Budget is null
            || string.IsNullOrWhiteSpace(task.Goal)) throw Invalid();
    }
    private static InvalidDataException Invalid() => new(UiText.Get("任务接口返回的范围或版本无效。"));
    private static CancellationTokenSource CreateDeadline(CancellationToken cancellationToken, int seconds)
    {
        var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(seconds));
        return deadline;
    }
    private async Task<T> ReadProjectionAsync<T>(string path, CancellationToken cancellationToken)
    {
        for (int attempt = 0; ; attempt++)
        {
            try
            {
                using var response = await _http.GetAsync(path, cancellationToken);
                return await ReadAsync<T>(response, cancellationToken);
            }
            catch (Exception error) when (attempt == 0 && !cancellationToken.IsCancellationRequested
                && (error is HttpRequestException || error is GatewayApiException gateway && (int)gateway.StatusCode >= 500))
            {
                // Retry a transient read once. Versioned writes and steering are never replayed by this client.
                // 临时读取故障仅重试一次；带版本控制和插话写入绝不由客户端自动重放。
                await Task.Delay(TimeSpan.FromMilliseconds(300), cancellationToken);
            }
        }
    }
    private static Task<T> ReadAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken) =>
        GatewayResponseReader.ReadAsync<T>(response, UiText.Get("任务接口返回了空响应。"),
            UiText.Get("任务接口返回 HTTP {0}。"), cancellationToken, JsonOptions);
    public void Dispose() { if (_ownsHttp) _http.Dispose(); }
}
