using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

namespace ShellMigrationUiSmoke;

/// <summary>
/// Serves a fixed synthetic catalog and model list on loopback; unexpected writes fail the fixture.
/// 在回环地址提供固定虚构目录与模型列表；意外写入会使夹具失败。
/// </summary>
internal sealed class ShellGatewayFixture : IAsyncDisposable
{
    private readonly HttpListener _listener = new();
    private readonly CancellationTokenSource _shutdown = new();
    private readonly Task _listening;
    private readonly List<Task> _responses = [];
    private readonly object _sync = new();
    private readonly string _fixtureDirectory;
    private readonly string _fixtureDataDirectory;
    private TaskCompletionSource? _initialCatalogGate;
    private int _catalogReads, _modelReads, _writes, _screenshotReads, _retrievalReads, _agentReads;

    private readonly List<AgentApprovalRequest> _approvalRequests = [];
    private TaskCompletionSource? _approvalGate;
    public bool EnableApprovals { get; set; }
    public int NextApprovalStatus { get; set; }
    public string? NextApprovalCode { get; set; }
    public AgentApprovalRequest[] ApprovalRequests { get { lock (_sync) return _approvalRequests.ToArray(); } }
    public void HoldApprovals() { lock (_sync) _approvalGate = new(TaskCreationOptions.RunContinuationsAsynchronously); }
    public void ReleaseApprovals() { lock (_sync) { _approvalGate?.TrySetResult(); _approvalGate = null; } }

    private TaskCompletionSource? _pausedRecoveryGate;
    private int _pausedReads, _streamRequests;
    private int _chromeOpenRequests;
    private int _activationRequests;
    private int _localModelReads;
    private ToolResultReference? _screenshotReference;
    private ToolResultResponse? _screenshotResult;
    private byte[]? _attachmentBytes;
    public string Address { get; }
    public int CatalogReads => Volatile.Read(ref _catalogReads);
    public int ModelReads => Volatile.Read(ref _modelReads);
    public int Writes => Volatile.Read(ref _writes);
    public int ScreenshotReads => Volatile.Read(ref _screenshotReads);
    public int RetrievalReads => Volatile.Read(ref _retrievalReads);
    public int AgentReads => Volatile.Read(ref _agentReads);

    public int PausedReads => Volatile.Read(ref _pausedReads);
    public int StreamRequests => Volatile.Read(ref _streamRequests);
    public int ChromeOpenRequests => Volatile.Read(ref _chromeOpenRequests);
    public int ActivationRequests => Volatile.Read(ref _activationRequests);
    public int LocalModelReads => Volatile.Read(ref _localModelReads);
    public bool EnableLocalModelReads { get; set; }
    public bool EnablePromptSends { get; set; }
    public bool EnableAttachmentImports { get; set; }
    public int AttachmentImports { get; private set; }
    public Guid? AttachmentMessageId { get; private set; }
    public ChatAttachment? ImportedAttachment { get; private set; }
    public bool EnableWorkspaceOpens { get; set; }
    public bool FailModelReads { get; set; }
    public Exception? Failure { get; private set; }
    public Guid ProjectId { get; } = Guid.NewGuid();
    public Guid SecondProjectId { get; } = Guid.NewGuid();
    public Guid WorkChatId { get; } = Guid.NewGuid();
    public Guid StandaloneChatId { get; } = Guid.NewGuid();
    public ConversationCatalog Catalog { get; }

    public ShellGatewayFixture(string directory)
    {
        _fixtureDirectory = Path.GetDirectoryName(Path.GetFullPath(directory))
            ?? throw new InvalidDataException("A fixture workspace must have an isolated parent directory.");
        _fixtureDataDirectory = Path.Combine(_fixtureDirectory, "Data");
        var work = new ProjectChatState { Id = WorkChatId, Title = "Work fixture title", Draft = "work original draft",
            Messages = [new() { Content = "Work synthetic question" }, new() { Role = "assistant", Content = "Work synthetic answer" }] };
        var standalone = new ProjectChatState { Id = StandaloneChatId, Title = "Chat fixture title", Draft = "chat original draft",
            Messages = [new() { Content = "Chat synthetic question" }, new() { Role = "assistant", Content = "Final visible answer $x^2$",
                Reasoning = "SECRET_HIDDEN_REASONING", AssistantSegments = [
                    new("stage", 1, 0, "commentary", "completed", "HIDDEN_SUCCESS_STAGE", "SECRET_STAGE_REASONING"),
                    new("final", 2, 1, "final_answer", "completed", "Final visible answer $x^2$", "")],
                ToolActivities = [new("hidden-call", "workspace.read_file", null, "completed", "HIDDEN_TOOL_RECEIPT")] }] };
        string secondDirectory = Path.Combine(directory, "Second workspace");
        Directory.CreateDirectory(secondDirectory);
        Catalog = new(7, [new() { Id = ProjectId, Name = "Work fixture project", FolderPath = directory, Chats = [work] },
            new() { Id = SecondProjectId, Name = "Second empty fixture project", FolderPath = secondDirectory }], [standalone]);
        using var reservation = new TcpListener(IPAddress.Loopback, 0);
        reservation.Start();
        int port = ((IPEndPoint)reservation.LocalEndpoint).Port;
        reservation.Stop();
        Address = $"http://127.0.0.1:{port}";
        _listener.Prefixes.Add(Address + "/");
        _listener.Start();
        _listening = ListenAsync();
    }

    public void HoldInitialCatalog()
    {
        lock (_sync)
        {
            if (CatalogReads != 0) throw new InvalidOperationException("The initial catalog must be held before creating the production Shell.");
            _initialCatalogGate ??= new(TaskCreationOptions.RunContinuationsAsynchronously);
        }
    }

    public void ReleaseInitialCatalog()
    {
        lock (_sync) _initialCatalogGate?.TrySetResult();
    }

    public void HoldPausedRecovery() => _pausedRecoveryGate = new(TaskCreationOptions.RunContinuationsAsynchronously);
    public void ReleasePausedRecovery() => _pausedRecoveryGate?.TrySetResult();

    public ToolResultReference SetScreenshotArchive(string png)
    {
        var archive = JsonSerializer.SerializeToElement(new
        {
            content = new[] { new { type = "image", mimeType = "image/png", data = png } }
        });
        byte[] bytes = Encoding.UTF8.GetBytes(archive.GetRawText());
        var reference = new ToolResultReference(Guid.NewGuid(), bytes.Length, Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant());
        lock (_sync)
        {
            _screenshotReference = reference;
            _screenshotResult = new(archive);
        }
        return reference;
    }

    private bool TryScreenshotResult(string path, out ToolResultResponse? result)
    {
        lock (_sync)
        {
            // Only the synthetic work receipt is readable; unknown conversations and resources still fail the fixture.
            // 仅允许读取虚构工作会话的这条回执；其它会话或资源请求仍使夹具失败。
            if (_screenshotReference is { } reference && path == $"/api/conversations/{WorkChatId:D}/tool-results/{reference.Id:D}")
            {
                Interlocked.Increment(ref _screenshotReads);
                result = _screenshotResult;
                return result is not null;
            }
        }
        result = null;
        return false;
    }

    private bool TryHistoryPage(HttpListenerRequest request, out ConversationHistoryPage? page)
    {
        string path = request.Url!.AbsolutePath;
        Guid conversationId;
        if (path == $"/api/conversations/{WorkChatId:D}/messages") conversationId = WorkChatId;
        else if (path == $"/api/conversations/{StandaloneChatId:D}/messages") conversationId = StandaloneChatId;
        else
        {
            page = null;
            return false;
        }

        // Opening a persisted fixture chat refreshes its recent page even when the catalog contains its full body.
        // 打开已保存的夹具聊天时，即使目录带有完整正文，正式界面仍会刷新最近历史页。
        // Serve only the two owned latest pages and the production initial/refresh bounds; cursors and unknown reads still fail.
        // 仅提供两个自有聊天的最近页及正式初始／刷新上限；游标和未知读取仍使夹具失败。
        var query = request.QueryString;
        bool initial = query["limit"] == "4" && query["maxBytes"] == "262144";
        bool refresh = query["limit"] == "40" && query["maxBytes"] == "1048576";
        if (query.Count != 2 || query.GetValues("limit")?.Length != 1 || query.GetValues("maxBytes")?.Length != 1 ||
            (!initial && !refresh))
            throw new InvalidOperationException("Unexpected fixture conversation history query: " + request.Url.Query);
        var chat = Catalog.Projects.SelectMany(project => project.Chats).Concat(Catalog.Chats)
            .Single(chat => chat.Id == conversationId);
        var messages = chat.Messages.ToList();
        long returnedBytes = JsonSerializer.SerializeToUtf8Bytes(messages).LongLength;
        page = new(conversationId, messages, messages.Count, false, null, $"fixture-history-{conversationId:D}-v1",
            returnedBytes, false, StartIndex: 0, EndIndex: messages.Count);
        return true;
    }

    private async Task RespondToApprovalAsync(HttpListenerContext context)
    {
        var request = await JsonSerializer.DeserializeAsync<AgentApprovalRequest>(context.Request.InputStream,
            new JsonSerializerOptions(JsonSerializerDefaults.Web), _shutdown.Token)
            ?? throw new InvalidDataException("An approval request is required.");
        if (request.ConversationId != WorkChatId && request.ConversationId != StandaloneChatId ||
            request.RequestId == Guid.Empty || request.ApprovalId == Guid.Empty || string.IsNullOrWhiteSpace(request.ToolCallId) ||
            request.Scope is not ("once" or "task") || request.Scope == "task" && !request.Approved)
            throw new InvalidDataException("Approval escaped its synthetic scope or exact call identity.");
        Task gate;
        int status;
        string? code;
        lock (_sync)
        {
            _approvalRequests.Add(request);
            gate = _approvalGate?.Task ?? Task.CompletedTask;
            status = NextApprovalStatus;
            code = NextApprovalCode;
            NextApprovalStatus = 0;
            NextApprovalCode = null;
        }
        // Hold only this owned response to observe production busy state and retries without a real model or tool.
        // 仅挂起自有审批响应，观察生产提交状态和重试，不调用真实模型或工具。
        await gate.WaitAsync(_shutdown.Token);
        context.Response.StatusCode = status == 0 ? 200 : status;
        context.Response.ContentType = "application/json";
        object result = status == 0
            ? new AgentApprovalResponse(request.Approved, request.ApprovalId, request.ToolCallId, request.Scope)
            : new { error = status == 409 ? "Approval expired." : "Synthetic approval unavailable.", code };
        await context.Response.OutputStream.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(result,
            new JsonSerializerOptions(JsonSerializerDefaults.Web)), _shutdown.Token);
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
        catch (Exception error) when (_shutdown.IsCancellationRequested && error is HttpListenerException or ObjectDisposedException) { }
        catch (Exception error) { Failure = error; }
    }

    private async Task RespondAsync(HttpListenerContext context)
    {
        try
        {
            object result;
            string path = context.Request.Url!.AbsolutePath;
            if (EnableApprovals && context.Request.HttpMethod == "POST" && path == "/api/agent/approvals")
            {
                await RespondToApprovalAsync(context);
                return;
            }
            if (context.Request.HttpMethod == "POST" && path == "/api/retrieval/activate")
            {
                await ActivateRetrievalAsync(context);
                return;
            }
            if (EnableAttachmentImports && context.Request.HttpMethod == "GET" && AttachmentImports == 2
                && ImportedAttachment is { } received && context.Request.QueryString["messageId"] == AttachmentMessageId?.ToString("D")
                && path == $"/api/conversations/{StandaloneChatId:D}/attachments/{received.Id:D}/content")
            {
                context.Response.ContentType = "image/png";
                await context.Response.OutputStream.WriteAsync(_attachmentBytes!, _shutdown.Token);
                return;
            }
            if (EnableAttachmentImports && context.Request.HttpMethod == "PUT" && path == "/api/conversations/catalog")
            {
                using var body = await JsonDocument.ParseAsync(context.Request.InputStream, cancellationToken: _shutdown.Token);
                var saved = Catalog.Chats.Single(chat => chat.Id == StandaloneChatId);
                foreach (var chat in body.RootElement.GetProperty("Chats").EnumerateArray())
                {
                    if (chat.GetProperty("Id").GetGuid() != StandaloneChatId)
                        throw new InvalidDataException("Attachment save escaped the synthetic conversation.");
                    var submitted = JsonSerializer.Deserialize<ProjectChatState>(chat.GetRawText())!;
                    foreach (var message in submitted.Messages.Where(message => message.Role == "user"))
                        if (!saved.Messages.Any(existing => existing.Id == message.Id)) saved.Messages.Add(message);
                }
                Interlocked.Increment(ref _writes);
                context.Response.ContentType = "application/json";
                await context.Response.OutputStream.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(Catalog), _shutdown.Token);
                return;
            }
            if (EnableAttachmentImports && context.Request.HttpMethod == "POST"
                && path == $"/api/conversations/{StandaloneChatId:D}/attachments/import")
            {
                await ImportAttachmentAsync(context);
                return;
            }
            if (EnablePromptSends && context.Request.HttpMethod == "POST" && path == "/api/chat/stream")
            {
                await ReplyToPromptAsync(context);
                return;
            }
            if (EnableWorkspaceOpens && context.Request.HttpMethod == "POST" && path == "/api/agent/workspace/chrome")
            {
                using var request = await JsonDocument.ParseAsync(context.Request.InputStream, cancellationToken: _shutdown.Token);
                if (request.RootElement.GetProperty("conversationId").GetGuid() != WorkChatId ||
                    request.RootElement.GetProperty("url").GetString() != Address + "/workspace-live")
                    throw new InvalidOperationException("Workspace opening escaped the synthetic conversation or URL.");
                Interlocked.Increment(ref _chromeOpenRequests);
                // Observe the actual production client's dispatch without starting Chrome or visiting a real website.
                // 观察生产客户端的真实派发；明确返回不可用，不启动 Chrome 或访问真实网站。
                context.Response.StatusCode = 503;
                context.Response.ContentType = "application/json";
                byte[] unavailable = JsonSerializer.SerializeToUtf8Bytes(new
                { code = "WORKSPACE_FIXTURE_UNAVAILABLE", error = "Synthetic browser unavailable; dispatch observed." });
                await context.Response.OutputStream.WriteAsync(unavailable, _shutdown.Token);
                return;
            }
            if (context.Request.HttpMethod != "GET")
            {
                Interlocked.Increment(ref _writes);
                throw new InvalidOperationException("UI-only Shell checks unexpectedly wrote to the mock gateway: " + path);
            }
            // Match the native health contract so readiness never attempts to start a real gateway.
            // 对齐原生健康合同，避免就绪探测将模拟服务误判为旧网关或启动真实网关。
            if (path == "/health") result = new { service = "kynxa-model-gateway", status = "ok", runtime = "rust", dataLayoutVersion = 1,
                conversationProtocol = 1, memoryProtocol = 1, contextProtocol = 3, agentProtocol = 5, officialToolsProtocol = 2,
                hostTerminalProtocol = 3, browserAutomationProtocol = 2, extensionStorageProtocol = 1, toolStreamProtocol = 3,
                retrievalProtocol = 1, modelDataHome = ApplicationPaths.ModelHome, extensionRoot = ExtensionPaths.Root };
            else if (path is "/api/tasks" or "/api/tasks/recoverable" && Guid.TryParse(context.Request.QueryString["conversationId"], out _))
                result = new TaskListResponse([], false, null);
            else if (path == "/api/tasks/paused" && Guid.TryParse(context.Request.QueryString["conversationId"], out _))
            {
                Interlocked.Increment(ref _pausedReads);
                if (_pausedRecoveryGate is { } pausedGate) await pausedGate.Task.WaitAsync(_shutdown.Token);
                result = new TaskPausedResponse([]);
            }
            else if (EnableWorkspaceOpens && path == $"/api/conversations/{WorkChatId:D}/messages")
            {
                var messages = Catalog.Projects.Single(project => project.Id == ProjectId).Chats.Single(chat => chat.Id == WorkChatId).Messages;
                result = new ConversationHistoryPage(WorkChatId, messages, messages.Count, false, null,
                    "synthetic-workspace-history", 0, false, StartIndex: 0, EndIndex: messages.Count);
            }
            else if (EnablePromptSends && path == $"/api/conversations/{StandaloneChatId:D}/messages")
            {
                var messages = Catalog.Chats.Single(chat => chat.Id == StandaloneChatId).Messages;
                result = new ConversationHistoryPage(StandaloneChatId, messages, messages.Count, false, null,
                    "synthetic-prompt-history", 0, false, StartIndex: 0, EndIndex: messages.Count);
            }
            else if (path == "/api/conversations/catalog")
            {
                int read = Interlocked.Increment(ref _catalogReads);
                Task gate;
                // Hold only the first catalog response; model and health requests remain independent.
                // 仅等待首次目录响应，不阻塞模型列表及健康检查请求。
                lock (_sync) gate = read == 1 ? _initialCatalogGate?.Task ?? Task.CompletedTask : Task.CompletedTask;
                await gate.WaitAsync(_shutdown.Token);
                result = Catalog;
            }
            else if (path == "/api/models")
            {
                Interlocked.Increment(ref _modelReads);
                if (FailModelReads)
                {
                    context.Response.StatusCode = 503;
                    result = new { error = "FIXTURE_MODEL_CATALOG_UNAVAILABLE" };
                }
                else result = new ModelListResponse([
                    new("fixture-provider", "Fixture provider", "https://models.example.invalid/v1",
                        ["fixture-model", "fixture-secondary"], false, "openai-completions"),
                    new("second-provider", "Second fixture provider", "https://second.example.invalid/v1",
                        ["fixture-model"], false, "openai-completions")]);
            }
            else if (EnableLocalModelReads && path == "/api/local-models" && string.IsNullOrEmpty(context.Request.Url.Query))
            {
                // Serve only the opt-in empty catalog; all local-model POST actions remain unexpected writes.
                // 仅按显式开关提供空目录读取；本地模型的所有 POST 动作仍作为意外写入拒绝。
                Interlocked.Increment(ref _localModelReads);
                result = new LocalModelsSnapshot { Enabled = false, InstallDirectory = Path.Combine(_fixtureDirectory, "LocalModels"),
                    LoadPolicy = "auto", Models = [] };
            }
            else if (TryAgentManagementRead(context.Request, out var agentManagement)) result = agentManagement!;
            else if (path == "/api/retrieval/settings")
            {
                Interlocked.Increment(ref _retrievalReads);
                result = new RetrievalSettingsDocument(1, 13, new(true, "auto", "fixture-embedding"),
                    new("off", "auto", "standard", "auto"), new(4 * 1024 * 1024, 16 * 1024 * 1024));
            }
            else if (path == "/api/retrieval/status")
            {
                Interlocked.Increment(ref _retrievalReads);
                result = new RetrievalStatus("sqlite", 0, 0, new("unavailable", "fixture-embedding", 384, false), []);
            }
            else if (path == "/api/retrieval/providers")
            {
                Interlocked.Increment(ref _retrievalReads);
                result = new RetrievalProvidersResponse([new("auto", "Automatic fixture provider", false)]);
            }
            else if (path == "/api/retrieval/sources" && string.IsNullOrEmpty(context.Request.Url.Query))
            {
                // Only global synthetic reads are supported; the fixture still refuses imports, rebuilds and all writes.
                // 仅提供全局虚构读取；夹具仍拒绝导入、重建及所有写入。
                Interlocked.Increment(ref _retrievalReads);
                result = new RetrievalSourcesResponse([]);
            }
            else if (TryHistoryPage(context.Request, out var history)) result = history!;
            else if (TryScreenshotResult(path, out var screenshot)) result = screenshot!;
            else throw new InvalidOperationException("Unexpected production Shell HTTP path: " + path);
            context.Response.ContentType = "application/json";
            byte[] bytes = JsonSerializer.SerializeToUtf8Bytes(result);
            await context.Response.OutputStream.WriteAsync(bytes, _shutdown.Token);
        }
        catch (Exception error) when (_shutdown.IsCancellationRequested && error is OperationCanceledException or HttpListenerException or IOException) { }
        catch (Exception error)
        {
            Failure = error;
            context.Response.StatusCode = 500;
        }
        finally { context.Response.Close(); }
    }

    private bool TryAgentManagementRead(HttpListenerRequest request, out object? result)
    {
        result = null;
        string path = request.Url!.AbsolutePath;
        if (path is not ("/api/agent/config" or "/api/agent/tools" or "/api/agent/skills" or "/api/agent/mcp/catalog")) return false;
        var query = request.QueryString;
        bool conversationQuery = path == "/api/agent/skills" && query.Count == 1 &&
            query.GetValues("conversationId") is { Length: 1 } values && Guid.TryParseExact(values[0], "D", out Guid conversationId) &&
            (conversationId == WorkChatId || conversationId == StandaloneChatId);
        if (!string.IsNullOrEmpty(request.Url.Query) && !conversationQuery)
            throw new InvalidOperationException("Unexpected fixture agent management query: " + request.Url.Query);
        // Empty skills require no preview, and disabled synthetic MCP configuration cannot start a real process.
        // 空技能列表无需预览；禁用的虚构 MCP 配置不会启动真实进程，所有写入仍由上层统一拒绝。
        result = path switch
        {
            "/api/agent/config" => new AgentConfig(1, 4,
                [new("fixture-disabled", "Fixture MCP", "fixture-mcp", [], false)], [ExtensionPaths.Root]),
            "/api/agent/tools" => new AgentToolsResponse([], Connections: []),
            "/api/agent/skills" => new AgentSkillsResponse([]),
            "/api/agent/mcp/catalog" => new McpCatalogResponse([], []),
            _ => throw new InvalidOperationException("Unexpected fixture agent management path.")
        };
        Interlocked.Increment(ref _agentReads);
        return true;
    }

    private async Task ActivateRetrievalAsync(HttpListenerContext context)
    {
        Guid? projectId;
        try
        {
            using var request = await JsonDocument.ParseAsync(context.Request.InputStream, cancellationToken: _shutdown.Token);
            var body = request.RootElement;
            if (!string.IsNullOrEmpty(context.Request.Url!.Query) || body.ValueKind != JsonValueKind.Object
                || body.EnumerateObject().Count() != 1 || !body.TryGetProperty("projectId", out var selected))
                throw new InvalidDataException("Unexpected fixture retrieval activation contract.");
            projectId = selected.ValueKind == JsonValueKind.Null ? null : selected.GetGuid();
            if (!Path.GetFullPath(ApplicationPaths.DataRoot).Equals(_fixtureDataDirectory, StringComparison.OrdinalIgnoreCase)
                || !Path.GetFullPath(Environment.GetEnvironmentVariable("KYNXA_DATA_ROOT") ?? string.Empty)
                    .Equals(_fixtureDataDirectory, StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("Retrieval activation escaped the fixture's temporary data directory.");
            if (projectId is Guid id)
            {
                var project = Catalog.Projects.SingleOrDefault(project => project.Id == id && (id == ProjectId || id == SecondProjectId))
                    ?? throw new InvalidDataException("Retrieval activation escaped the fixture's project scope.");
                var paths = project.Mounts.Select(mount => mount.Path)
                    .Concat(project.FolderPath is { } defaultDirectory ? [defaultDirectory] : []);
                foreach (string workspaceDirectory in paths)
                    if (!Path.GetFullPath(workspaceDirectory).StartsWith(_fixtureDirectory + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
                        || !Directory.Exists(workspaceDirectory))
                        throw new InvalidDataException("Retrieval activation referenced a nonfixture workspace directory.");
            }
        }
        catch
        {
            Interlocked.Increment(ref _writes);
            throw;
        }
        // Activation is a scoped scheduling hint, not a catalog/chat mutation; this fixture never starts an index or model.
        // 激活是带范围的调度提示，不是目录或聊天写入；夹具只确认接收，不启动索引或模型。
        Interlocked.Increment(ref _activationRequests);
        context.Response.StatusCode = 202;
        context.Response.ContentType = "application/json";
        await context.Response.OutputStream.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(new
            { projectId, state = projectId is null ? "idle" : "queued", queued = projectId is not null }), _shutdown.Token);
    }

    private async Task ReplyToPromptAsync(HttpListenerContext context)
    {
        var options = new JsonSerializerOptions(JsonSerializerDefaults.Web);
        var request = await JsonSerializer.DeserializeAsync<ChatRequest>(context.Request.InputStream, options, _shutdown.Token)
            ?? throw new InvalidDataException("Missing synthetic prompt.");
        if (request.ConversationId != StandaloneChatId || request.UserMessageId is null || request.RequestId is null)
            throw new InvalidDataException("Prompt escaped the synthetic conversation or lost its stable IDs.");
        if (request.AttachmentIds is { Length: > 0 } && (AttachmentImports != 2
            || request.UserMessageId != AttachmentMessageId || request.AttachmentIds.Single() != ImportedAttachment?.Id))
            throw new InvalidDataException("Screenshot reached the model before reception or lost its stable IDs.");
        Interlocked.Increment(ref _writes);
        Interlocked.Increment(ref _streamRequests);
        context.Response.ContentType = "text/event-stream";
        context.Response.SendChunked = true;
        string answer = "Fixture reply: " + request.Message;
        foreach (var update in new[]
        {
            new ChatStreamEvent("started", request.ConversationId, request.RequestId.Value, DateTimeOffset.UtcNow),
            new ChatStreamEvent("text_delta", request.ConversationId, request.RequestId.Value, DateTimeOffset.UtcNow, Delta: answer),
            new ChatStreamEvent("completed", request.ConversationId, request.RequestId.Value, DateTimeOffset.UtcNow, Content: answer)
        })
        {
            byte[] bytes = Encoding.UTF8.GetBytes("event: " + update.Type + "\r\ndata: " + JsonSerializer.Serialize(update, options) + "\r\n\r\n");
            await context.Response.OutputStream.WriteAsync(bytes, _shutdown.Token);
            await context.Response.OutputStream.FlushAsync(_shutdown.Token);
        }
    }

    private async Task ImportAttachmentAsync(HttpListenerContext context)
    {
        using var body = await JsonDocument.ParseAsync(context.Request.InputStream, cancellationToken: _shutdown.Token);
        Guid messageId = body.RootElement.GetProperty("messageId").GetGuid();
        var file = body.RootElement.GetProperty("files").EnumerateArray().Single();
        Guid attachmentId = file.GetProperty("id").GetGuid();
        string path = file.GetProperty("path").GetString()!;
        if (Path.GetFullPath(path).StartsWith(Path.GetFullPath(ApplicationPaths.DataRoot).TrimEnd(Path.DirectorySeparatorChar)
            + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
            || !Catalog.Chats.Single(chat => chat.Id == StandaloneChatId).Messages.Any(message => message.Id == messageId))
            throw new InvalidDataException("Clipboard source is private or the user message was not saved before import.");
        byte[] bytes = await File.ReadAllBytesAsync(path, _shutdown.Token);
        if (bytes.Length < 8 || !bytes.AsSpan(0, 8).SequenceEqual(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 }))
            throw new InvalidDataException("Clipboard pixels were not encoded as PNG.");
        if (AttachmentMessageId is { } previous && (previous != messageId || ImportedAttachment?.Id != attachmentId))
            throw new InvalidDataException("Retry created a different message or attachment.");
        AttachmentMessageId = messageId;
        ImportedAttachment = new(attachmentId, file.GetProperty("name").GetString()!, "image/png", bytes.Length,
            Sha256: Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant());
        _attachmentBytes = bytes;
        AttachmentImports++;
        Interlocked.Increment(ref _writes);
        context.Response.ContentType = "application/json";
        // Fail once to verify that production retry retains the original draft and waits for a real receipt.
        // 首次接收失败，用于核对生产重试保留原草稿并等待真实接收回执。
        object result;
        if (AttachmentImports == 1)
        {
            context.Response.StatusCode = 503;
            result = new { code = "ATTACHMENT_FIXTURE_UNAVAILABLE", error = "Synthetic attachment reception unavailable." };
        }
        else result = new { attachments = new[] { ImportedAttachment } };
        await context.Response.OutputStream.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(result,
            new JsonSerializerOptions(JsonSerializerDefaults.Web)), _shutdown.Token);
    }

    public async ValueTask DisposeAsync()
    {
        _shutdown.Cancel();
        ReleaseInitialCatalog();
        ReleasePausedRecovery();
        _listener.Stop();
        await _listening;
        Task[] responses;
        lock (_sync) responses = _responses.ToArray();
        await Task.WhenAll(responses);
        _listener.Close();
        _shutdown.Dispose();
    }
}
