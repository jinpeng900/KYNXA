using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record WorkspaceTerminalSession(string SessionId, int Cols, int Rows);
public sealed record WorkspaceTerminalOutput(string Output, ulong NextCursor, bool HasMore, bool Exited,
    int? ExitCode = null, bool Truncated = false);

/// <summary>
/// The gateway owns the PTY and resolves the working directory; this client carries terminal I/O only.
/// 网关拥有伪终端并解析正式工作目录；客户端只传递终端输入、输出和尺寸。
/// </summary>
public sealed class WorkspaceTerminalApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http = new() { BaseAddress = ModelGatewayService.Address, Timeout = Timeout.InfiniteTimeSpan };

    public async Task<WorkspaceTerminalSession> OpenAsync(Guid conversationId, int cols, int rows,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(45));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token);
        using var response = await _http.PostAsJsonAsync("/api/agent/workspace/terminal/open",
            new { conversationId, cols, rows }, JsonOptions, deadline.Token);
        return await GatewayResponseReader.ReadAsync<WorkspaceTerminalSession>(response,
            UiText.Get("终端启动结果为空。"), UiText.Get("终端启动失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public async Task<WorkspaceTerminalOutput> ReadAsync(Guid conversationId, string sessionId, ulong after,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(25));
        using var response = await _http.PostAsJsonAsync("/api/agent/workspace/terminal/action",
            new { conversationId, sessionId, action = "read", after }, JsonOptions, deadline.Token);
        return await GatewayResponseReader.ReadAsync<WorkspaceTerminalOutput>(response,
            UiText.Get("终端输出返回为空。"), UiText.Get("终端读取失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public Task WriteAsync(Guid conversationId, string sessionId, string input, CancellationToken cancellationToken = default) =>
        ActionAsync(new { conversationId, sessionId, action = "write", input }, cancellationToken);

    public Task ResizeAsync(Guid conversationId, string sessionId, int cols, int rows, CancellationToken cancellationToken = default) =>
        ActionAsync(new { conversationId, sessionId, action = "resize", cols, rows }, cancellationToken);

    public Task CloseAsync(Guid conversationId, string sessionId, CancellationToken cancellationToken = default) =>
        ActionAsync(new { conversationId, sessionId, action = "close" }, cancellationToken, allowMissing: true);

    private async Task ActionAsync(object payload, CancellationToken cancellationToken, bool allowMissing = false)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(15));
        using var response = await _http.PostAsJsonAsync("/api/agent/workspace/terminal/action", payload, JsonOptions, deadline.Token);
        if (allowMissing && response.StatusCode is System.Net.HttpStatusCode.NotFound or System.Net.HttpStatusCode.Gone) return;
        await GatewayResponseReader.EnsureSuccessAsync(response, UiText.Get("终端操作失败（HTTP {0}）。"), deadline.Token);
    }

    public void Dispose() => _http.Dispose();
}
