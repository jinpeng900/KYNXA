using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using System.Text.Json.Nodes;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

public sealed record ConversationCatalog(int Revision, List<ProjectState> Projects, List<ProjectChatState> Chats);
public sealed record ConversationHistoryPage(Guid ConversationId, List<ChatMessageState> Messages,
    int TotalMessages, bool HasMore, string? NextCursor, string? Snapshot, long ReturnedBytes, bool ByteBudgetExceeded,
    string? NewerCursor = null, bool HasNewer = false, int StartIndex = 0, int EndIndex = 0);

/// <summary>
/// The gateway owns conversation persistence. The desktop keeps only its current UI projection.
/// 网关拥有正式聊天持久化；桌面只保留当前展示投影。
/// </summary>
public sealed class ProjectStore(string dataDirectory) : IDisposable
{
    // Bound cold catalog/history recovery time while respecting caller cancellation.
    // 冷启动恢复及正式历史校验有等待上限，仍支持调用方取消。
    private readonly HttpClient _httpClient = GatewayHttpClient.Create(ModelGatewayService.Address, TimeSpan.FromMinutes(2));
    private readonly SemaphoreSlim _requestGate = new(1, 1);
    private static readonly JsonSerializerOptions JsonOptions = new() { PropertyNameCaseInsensitive = true };
    private int? _revision;
    private JsonElement? _catalogMetadata;
    private JsonElement? _savedCatalogMetadata;
    private Dictionary<Guid, JsonElement> _workBindings = [];
    private IReadOnlyList<ProjectState> _loadedProjects = [];
    private readonly HashSet<(Guid Chat, Guid Message)> _knownUserMessages = [];
    private readonly object _knownUserMessagesLock = new();

    public Task<ConversationCatalog> LoadAsync(CancellationToken cancellationToken = default) =>
        LoadCatalogAsync(includeMessages: true, cancellationToken);

    /// <summary>
    /// Read metadata and paged history from the gateway; load older bodies on demand.
    /// 元信息和历史页均来自网关，按需读旧正文，不另存聊天状态。
    /// </summary>
    public Task<ConversationCatalog> LoadMetadataAsync(CancellationToken cancellationToken = default) =>
        LoadCatalogAsync(includeMessages: false, cancellationToken);

    private async Task<ConversationCatalog> LoadCatalogAsync(bool includeMessages, CancellationToken cancellationToken)
    {
        // Headers-only transport avoids duplicating the JSON body; its deadline also covers streamed decoding.
        // 仅先接收响应头，避免完整 JSON 缓冲副本；总期限同时覆盖后续流式解码。
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(_httpClient.Timeout);
        cancellationToken = deadline.Token;
        await _requestGate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
            string endpoint = includeMessages ? "/api/conversations/catalog" : "/api/conversations/catalog?includeMessages=false";
            using var response = await _httpClient.GetAsync(endpoint, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
            var catalog = await ReadAsync(response, cancellationToken).ConfigureAwait(false);
            AcceptCatalog(catalog);
            _loadedProjects = catalog.Projects;
            return catalog;
        }
        finally { _requestGate.Release(); }
    }

    public async Task<ConversationHistoryPage> LoadHistoryPageAsync(Guid conversationId, string? cursor = null,
        int limit = 200, CancellationToken cancellationToken = default, int? maximumBytes = null)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(_httpClient.Timeout);
        cancellationToken = deadline.Token;
        await ModelGatewayService.EnsureReadyAsync(cancellationToken).ConfigureAwait(false);
        string endpoint = $"/api/conversations/{conversationId:D}/messages?limit={limit}";
        if (cursor is not null) endpoint += $"&cursor={Uri.EscapeDataString(cursor)}";
        if (maximumBytes is int bytes) endpoint += $"&maxBytes={bytes}";
        using var response = await _httpClient.GetAsync(endpoint, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        var page = await GatewayResponseReader.ReadAsync<ConversationHistoryPage>(response,
            UiText.Get("会话存储接口返回了空目录。"), UiText.Get("会话存储接口返回 HTTP {0}。"),
            cancellationToken, JsonOptions).ConfigureAwait(false);
        if (page.ConversationId != conversationId)
            throw new InvalidDataException("Conversation history identity mismatch. / 聊天历史身份不匹配。");
        AcceptHistoryPage(page);
        return page;
    }

    // Restore persisted message IDs when reusing a page evicted from the display window.
    // 复用历史页时恢复已保存的消息 ID，避免将淘汰的旧消息当成新提交。
    internal void AcceptHistoryPage(ConversationHistoryPage page)
    {
        lock (_knownUserMessagesLock)
            foreach (var message in page.Messages.Where(message => message.Role == "user"))
                _knownUserMessages.Add((page.ConversationId, message.Id));
    }

    /// <summary>
    /// Acknowledge the user's stable ID only after the gateway confirms its durable submission.
    /// 网关确认持久化后才确认用户消息 ID，后续保存不再上传；仅 started 事件不足为据。
    /// </summary>
    internal void AcceptSubmittedMessage(Guid conversationId, Guid userMessageId)
    {
        lock (_knownUserMessagesLock) _knownUserMessages.Add((conversationId, userMessageId));
    }

    /// <summary>
    /// Projection eviction cannot drop user submissions that have not reached durable storage.
    /// 展示缓存淘汰不能丢弃尚未进入正式存储的用户消息。
    /// </summary>
    public bool CanReleaseHistory(ProjectChatState chat)
    {
        lock (_knownUserMessagesLock)
            return chat.IsPersisted && chat.Messages.Where(message => message.Role == "user")
                .All(message => _knownUserMessages.Contains((chat.Id, message.Id)));
    }

    public void ReleaseHistoryProjection(Guid conversationId)
    {
        lock (_knownUserMessagesLock) _knownUserMessages.RemoveWhere(entry => entry.Chat == conversationId);
    }

    public List<ChatMessageState> UnpersistedHistoryTail(ProjectChatState chat)
    {
        lock (_knownUserMessagesLock)
        {
            int first = chat.Messages.FindIndex(message => message.Role == "user"
                && !_knownUserMessages.Contains((chat.Id, message.Id)));
            return first < 0 ? [] : chat.Messages.Skip(first).ToList();
        }
    }

    public void RetainHistoryProjection(Guid conversationId, IReadOnlySet<Guid> retainedIds)
    {
        lock (_knownUserMessagesLock)
            _knownUserMessages.RemoveWhere(entry => entry.Chat == conversationId && !retainedIds.Contains(entry.Message));
    }

    // Capture snapshots before the first await: UI collections can change during a request.
    // 首个 await 前捕获快照；请求执行时 UI 集合可能继续变化。
    private List<ProjectState> SnapshotProjects(IReadOnlyList<ProjectState> projects) => projects.Select(project => new ProjectState
        {
            Id = project.Id, Name = project.Name, FolderPath = project.FolderPath,
            Mounts = project.Mounts.ToList(), DefaultMountId = project.DefaultMountId,
            IsPinned = project.IsPinned, IsArchived = project.IsArchived, IsFolderlessWorkspace = project.IsFolderlessWorkspace,
            Chats = project.Chats.Where(chat => chat.CanPersist).Select(SnapshotChat).ToList()
        }).ToList();

    public async Task SaveAsync(IReadOnlyList<ProjectState> projects)
    {
        var submitted = projects.SelectMany(project => project.Chats).Where(chat => chat.CanPersist).ToArray();
        await SaveSnapshotAsync(JsonSerializer.SerializeToElement(SnapshotProjects(projects), JsonOptions), null, projects);
        foreach (var chat in submitted) chat.IsPersisted = true;
    }

    public async Task SaveChatsAsync(IEnumerable<ProjectChatState> chats)
    {
        var submitted = chats.Where(chat => chat.CanPersist).ToArray();
        await SaveSnapshotAsync(null, JsonSerializer.SerializeToElement(submitted.Select(SnapshotChat), JsonOptions));
        foreach (var chat in submitted) chat.IsPersisted = true;
    }

    /// <summary>
    /// Save both sidebar scopes in one revision; unchanged exit snapshots need no network write.
    /// 两类侧栏范围在同一版本内保存；退出快照未变化时无需网络写入。
    /// </summary>
    public Task SaveCatalogAsync(IReadOnlyList<ProjectState> projects, IEnumerable<ProjectChatState> chats,
        CancellationToken cancellationToken = default)
    {
        var projectSnapshots = SnapshotProjects(projects);
        var chatSnapshots = chats.Where(chat => chat.CanPersist).Select(SnapshotChat).ToList();
        var metadata = CatalogMetadata(new(0, projectSnapshots, chatSnapshots), includeFolders: true);
        bool hasNewMessages = chatSnapshots.Concat(projectSnapshots.SelectMany(project => project.Chats))
            .Any(chat => chat.Messages.Count > 0);
        return SaveSnapshotAsync(JsonSerializer.SerializeToElement(projectSnapshots, JsonOptions),
            JsonSerializer.SerializeToElement(chatSnapshots, JsonOptions), projects, cancellationToken,
            hasNewMessages ? null : metadata);
    }

    private ProjectChatState SnapshotChat(ProjectChatState chat)
    {
        lock (_knownUserMessagesLock)
            return new()
            {
                Id = chat.Id, Title = chat.Title, Draft = chat.Draft, IsSample = chat.IsSample,
                IsPinned = chat.IsPinned, IsArchived = chat.IsArchived,
                // Runtime owns assistant content. Metadata updates send only new submitted user messages.
                // 助手内容由运行时负责；元数据更新只发送新提交的用户消息。
                Messages = chat.Messages.Where(message => message.Role == "user" && !_knownUserMessages.Contains((chat.Id, message.Id))).ToList()
            };
    }

    private void AcceptCatalog(ConversationCatalog catalog)
    {
        foreach (var chat in catalog.Chats.Concat(catalog.Projects.SelectMany(project => project.Chats)))
            chat.IsPersisted = true;
        _revision = catalog.Revision;
        _catalogMetadata = CatalogMetadata(catalog);
        _savedCatalogMetadata = CatalogMetadata(catalog, includeFolders: true);
        _workBindings = catalog.Projects.ToDictionary(project => project.Id, WorkBinding);
        lock (_knownUserMessagesLock)
            foreach (var chat in catalog.Chats.Concat(catalog.Projects.SelectMany(project => project.Chats)))
                foreach (var message in chat.Messages.Where(message => message.Role == "user"))
                    _knownUserMessages.Add((chat.Id, message.Id));
    }

    private async Task SaveSnapshotAsync(JsonElement? projects, JsonElement? chats, IReadOnlyList<ProjectState>? projectReferences = null,
        CancellationToken cancellationToken = default, JsonElement? unchangedMetadata = null)
    {
        await _requestGate.WaitAsync(cancellationToken);
        try
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (unchangedMetadata is { } snapshot && _savedCatalogMetadata is { } savedMetadata && JsonElement.DeepEquals(snapshot, savedMetadata)) return;
            await ModelGatewayService.EnsureReadyAsync(cancellationToken);
            if (_revision is null) throw new InvalidOperationException(UiText.Get("会话目录尚未加载，请重新打开 KYNXA。"));
            var snapshotBindings = projects?.EnumerateArray().Select(project => project.Deserialize<ProjectState>(JsonOptions)!)
                .ToDictionary(project => project.Id, WorkBinding) ?? new Dictionary<Guid, JsonElement>(_workBindings);
            var payload = new Dictionary<string, object> { ["Revision"] = _revision.Value };
            if (projects is { } projectData) payload["Projects"] = projectData;
            if (chats is { } chatData) payload["Chats"] = chatData;
            using var response = await _httpClient.PutAsJsonAsync("/api/conversations/catalog?includeMessages=false", payload, JsonOptions, cancellationToken);
            ConversationCatalog catalog;
            if (response.StatusCode == HttpStatusCode.Conflict)
            {
                using var refreshed = await _httpClient.GetAsync("/api/conversations/catalog?includeMessages=false", cancellationToken);
                var latest = await ReadAsync(refreshed, cancellationToken);
                var mergedProjects = MergeWorkFolders(latest, projects);
                if (mergedProjects is null) await ReadAsync(response, cancellationToken);
                payload["Revision"] = latest.Revision;
                if (projects is not null) payload["Projects"] = mergedProjects!.Value;
                using var retry = await _httpClient.PutAsJsonAsync("/api/conversations/catalog?includeMessages=false", payload, JsonOptions, cancellationToken);
                catalog = await ReadAsync(retry, cancellationToken);
            }
            else catalog = await ReadAsync(response, cancellationToken);
            // Apply mount receipts only to unchanged submissions, preserving later local edits.
            // 挂载回执只更新提交后未改的绑定，保留后续新增的根和默认目录修改。
            var savedBindings = catalog.Projects.ToDictionary(project => project.Id);
            foreach (var project in projectReferences ?? _loadedProjects)
                if (snapshotBindings.TryGetValue(project.Id, out var submitted) && JsonElement.DeepEquals(WorkBinding(project), submitted) &&
                    savedBindings.TryGetValue(project.Id, out var saved))
                { project.FolderPath = saved.FolderPath; project.Mounts = saved.Mounts.ToList(); project.DefaultMountId = saved.DefaultMountId; }
            if (projectReferences is not null) _loadedProjects = projectReferences;
            AcceptCatalog(catalog);
            // Successful metadata receipts acknowledge submitted user IDs without resending history.
            // 元信息保存成功时一并确认用户消息 ID，无需重传历史正文。
            lock (_knownUserMessagesLock)
                foreach (var submittedChat in new[] { projects, chats }.Where(value => value is not null)
                    .SelectMany(value => value!.Value.EnumerateArray())
                    .SelectMany(value => value.TryGetProperty("Chats", out var children) ? children.EnumerateArray().ToArray() : [value]))
                    foreach (var submittedMessage in submittedChat.GetProperty("Messages").EnumerateArray())
                        _knownUserMessages.Add((submittedChat.GetProperty("Id").GetGuid(), submittedMessage.GetProperty("Id").GetGuid()));
        }
        finally { _requestGate.Release(); }
    }

    /// <summary>
    /// Rebase only concurrent work-folder changes; all other metadata or competing folder edits retain conflict semantics.
    /// 仅合并并发工作目录变更，其他元信息变更和同一目录的竞争编辑仍保留版本冲突。
    /// </summary>
    private JsonElement? MergeWorkFolders(ConversationCatalog latest, JsonElement? projects)
    {
        if (_catalogMetadata is not { } baseline || !JsonElement.DeepEquals(baseline, CatalogMetadata(latest))) return null;
        if (projects is null) return JsonSerializer.SerializeToElement(Array.Empty<object>());
        var next = JsonNode.Parse(projects.Value.GetRawText())!.AsArray();
        var latestBindings = latest.Projects.ToDictionary(project => project.Id, WorkBinding);
        foreach (var node in next)
        {
            var project = node!.AsObject();
            var id = project["Id"]!.GetValue<Guid>();
            if (!_workBindings.TryGetValue(id, out var original) || !latestBindings.TryGetValue(id, out var current) || JsonElement.DeepEquals(original, current))
                continue;
            var proposed = WorkBinding(project.Deserialize<ProjectState>(JsonOptions)!);
            if (!JsonElement.DeepEquals(proposed, original) && !JsonElement.DeepEquals(proposed, current)) return null;
            foreach (string key in new[] { "FolderPath", "Mounts", "DefaultMountId" })
                project[key] = JsonNode.Parse(current.GetProperty(key).GetRawText());
        }
        return JsonSerializer.SerializeToElement(next, JsonOptions);
    }

    private static JsonElement WorkBinding(ProjectState project) => JsonSerializer.SerializeToElement(new
    { project.FolderPath, project.Mounts, project.DefaultMountId }, JsonOptions);

    private static JsonElement CatalogMetadata(ConversationCatalog catalog, bool includeFolders = false)
    {
        static object ChatMetadata(ProjectChatState chat) => new
        { chat.Id, chat.Title, chat.Draft, chat.IsSample, chat.IsPinned, chat.IsArchived };
        return JsonSerializer.SerializeToElement(new
        {
            Projects = catalog.Projects.Select(project => new
            {
                project.Id, project.Name, project.IsPinned, project.IsArchived, project.IsFolderlessWorkspace,
                FolderPath = includeFolders ? project.FolderPath : null,
                Mounts = includeFolders ? project.Mounts : null,
                DefaultMountId = includeFolders ? project.DefaultMountId : null,
                Chats = project.Chats.Select(ChatMetadata)
            }),
            Chats = catalog.Chats.Select(ChatMetadata)
        }, JsonOptions);
    }

    private static Task<ConversationCatalog> ReadAsync(HttpResponseMessage response, CancellationToken cancellationToken) =>
        GatewayResponseReader.ReadAsync<ConversationCatalog>(response, UiText.Get("会话存储接口返回了空目录。"),
            UiText.Get("会话存储接口返回 HTTP {0}。"), cancellationToken, JsonOptions,
            UiText.Get("聊天目录已在其他窗口中更新，请重新打开 KYNXA 后重试。"));

    public string CreateManagedFolder(Guid projectId)
    {
        string folder = Path.Combine(dataDirectory, "Projects", projectId.ToString("N"));
        Directory.CreateDirectory(folder);
        return folder;
    }

    public void Dispose() => _httpClient.Dispose();
}
