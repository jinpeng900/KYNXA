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
    private TaskCompletionSource? _initialCatalogGate;
    private int _catalogReads, _modelReads, _writes, _screenshotReads, _retrievalReads;
    private ToolResultReference? _screenshotReference;
    private ToolResultResponse? _screenshotResult;
    public string Address { get; }
    public int CatalogReads => Volatile.Read(ref _catalogReads);
    public int ModelReads => Volatile.Read(ref _modelReads);
    public int Writes => Volatile.Read(ref _writes);
    public int ScreenshotReads => Volatile.Read(ref _screenshotReads);
    public int RetrievalReads => Volatile.Read(ref _retrievalReads);
    public bool FailModelReads { get; set; }
    public Exception? Failure { get; private set; }
    public Guid ProjectId { get; } = Guid.NewGuid();
    public Guid SecondProjectId { get; } = Guid.NewGuid();
    public Guid WorkChatId { get; } = Guid.NewGuid();
    public Guid StandaloneChatId { get; } = Guid.NewGuid();
    public ConversationCatalog Catalog { get; }

    public ShellGatewayFixture(string directory)
    {
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
            if (context.Request.HttpMethod != "GET")
            {
                Interlocked.Increment(ref _writes);
                throw new InvalidOperationException("UI-only Shell checks unexpectedly wrote to the mock gateway: " + path);
            }
            if (path == "/health") result = new { service = "kynxa-model-gateway", status = "ok", dataLayoutVersion = 1,
                conversationProtocol = 1, memoryProtocol = 1, contextProtocol = 3, agentProtocol = 5, officialToolsProtocol = 2,
                hostTerminalProtocol = 3, browserAutomationProtocol = 2, extensionStorageProtocol = 1, toolStreamProtocol = 3,
                retrievalProtocol = 1 };
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

    public async ValueTask DisposeAsync()
    {
        _shutdown.Cancel();
        ReleaseInitialCatalog();
        _listener.Stop();
        await _listening;
        Task[] responses;
        lock (_sync) responses = _responses.ToArray();
        await Task.WhenAll(responses);
        _listener.Close();
        _shutdown.Dispose();
    }
}
